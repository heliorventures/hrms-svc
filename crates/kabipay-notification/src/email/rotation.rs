//! Explicit, tenant-bound operational rotation. No key or credential bytes are reportable.
use super::keyring::{valid_key_id, Keyring};
use super::{
    crypto::{open_email_credential, seal_email_credential},
    settings::stored_credential,
};
use kabipay_common::private_events::{append_audit, AuditRecordInput};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RotationMode {
    Preview,
    Apply,
}
#[derive(Clone, Debug)]
pub struct RotationRequest {
    pub tenant_id: Uuid,
    pub expected_database: String,
    pub expected_schema: String,
    pub expected_version: i64,
    pub current_key_id: String,
    pub new_key_id: String,
    pub mode: RotationMode,
}
#[derive(Debug, thiserror::Error)]
pub enum RotationError {
    #[error("EMAIL_ROTATION_CONFIGURATION")]
    Configuration,
    #[error("EMAIL_ROTATION_BINDING")]
    Binding,
    #[error("EMAIL_ROTATION_VERSION_CONFLICT")]
    VersionConflict,
    #[error("EMAIL_ROTATION_KEY_UNAVAILABLE")]
    KeyUnavailable,
    #[error("EMAIL_ROTATION_CREDENTIAL_UNAVAILABLE")]
    CredentialUnavailable,
    #[error("EMAIL_ROTATION_DATABASE")]
    Database,
    #[error("EMAIL_ROTATION_ACTIVE_DELIVERY")]
    ActiveDelivery,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RotationState {
    Preview,
    Rotated,
    AlreadyCurrent,
    NoCredential,
}
#[derive(Debug, Serialize)]
pub struct RotationReport {
    pub tenant_id: Uuid,
    pub settings_id: Uuid,
    pub settings_version: i64,
    pub state: RotationState,
    pub changed_credentials: u8,
    pub verified_credentials: u8,
    pub old_key_retirement_authorized: bool,
}
fn identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value.as_bytes()[0].is_ascii_lowercase()
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'_')
}
impl RotationRequest {
    pub fn validate(&self) -> Result<(), RotationError> {
        if self.tenant_id.is_nil()
            || !identifier(&self.expected_database)
            || !self.expected_database.starts_with("company_")
            || !identifier(&self.expected_schema)
            || self.expected_schema == "public"
            || self.expected_schema.starts_with("pg_")
            || self.expected_version <= 0
            || self.expected_version == i64::MAX
            || !valid_key_id(&self.current_key_id)
            || !valid_key_id(&self.new_key_id)
            || self.current_key_id == self.new_key_id
        {
            return Err(RotationError::Configuration);
        }
        Ok(())
    }
}
pub async fn rotate_email_credential(
    db: &DatabaseConnection,
    request: &RotationRequest,
    keys: &Keyring,
) -> Result<RotationReport, RotationError> {
    request.validate()?;
    // The tool never edits the keyring. Both keys must remain available, including on a resume.
    keys.key(request.tenant_id, &request.current_key_id)
        .map_err(|_| RotationError::KeyUnavailable)?;
    keys.key(request.tenant_id, &request.new_key_id)
        .map_err(|_| RotationError::KeyUnavailable)?;
    if keys
        .active_id(request.tenant_id)
        .map_err(|_| RotationError::KeyUnavailable)?
        != request.new_key_id
    {
        return Err(RotationError::KeyUnavailable);
    }
    let tx = db.begin().await.map_err(|_| RotationError::Database)?;
    if request.mode == RotationMode::Preview {
        tx.execute_unprepared("SET TRANSACTION READ ONLY")
            .await
            .map_err(|_| RotationError::Database)?;
    }
    tx.execute_unprepared("SET LOCAL lock_timeout='5s'; SET LOCAL statement_timeout='10s'")
        .await
        .map_err(|_| RotationError::Database)?;
    let binding = tx.query_one(Statement::from_string(DbBackend::Postgres,
        "SELECT current_database()::text AS database,current_schema()::text AS schema,current_user::text AS role".to_owned()))
        .await.map_err(|_| RotationError::Database)?.ok_or(RotationError::Binding)?;
    let database: String = binding
        .try_get("", "database")
        .map_err(|_| RotationError::Binding)?;
    let schema: String = binding
        .try_get("", "schema")
        .map_err(|_| RotationError::Binding)?;
    let role: String = binding
        .try_get("", "role")
        .map_err(|_| RotationError::Binding)?;
    if database != request.expected_database
        || schema != request.expected_schema
        || role != "hrms_company_notification"
    {
        return Err(RotationError::Binding);
    }
    let lock = if request.mode == RotationMode::Apply {
        " FOR UPDATE"
    } else {
        ""
    };
    let row = tx.query_one(Statement::from_string(DbBackend::Postgres,
        format!("SELECT id,tenant_id,version,credential_cipher,credential_nonce,credential_key_id FROM tenant_email_settings WHERE singleton{lock}")))
        .await.map_err(|_| RotationError::Database)?.ok_or(RotationError::Binding)?;
    let tenant: Uuid = row
        .try_get("", "tenant_id")
        .map_err(|_| RotationError::Binding)?;
    if tenant != request.tenant_id {
        return Err(RotationError::Binding);
    }
    let id: Uuid = row.try_get("", "id").map_err(|_| RotationError::Binding)?;
    let version: i64 = row
        .try_get("", "version")
        .map_err(|_| RotationError::Binding)?;
    let mut report = RotationReport {
        tenant_id: tenant,
        settings_id: id,
        settings_version: version,
        state: RotationState::NoCredential,
        changed_credentials: 0,
        verified_credentials: 0,
        old_key_retirement_authorized: false,
    };
    let credential = stored_credential(&row).map_err(|_| RotationError::CredentialUnavailable)?;
    let Some(credential) = credential else {
        tx.rollback().await.map_err(|_| RotationError::Database)?;
        return Ok(report);
    };
    if credential.key_id != request.current_key_id && credential.key_id != request.new_key_id {
        return Err(RotationError::CredentialUnavailable);
    }
    let secret = open_email_credential(tenant, id, &credential, keys)
        .map_err(|_| RotationError::CredentialUnavailable)?;
    report.verified_credentials = 1;
    if credential.key_id == request.new_key_id {
        // A completed company can be verified independently of later administrator versions.
        report.state = RotationState::AlreadyCurrent;
        tx.rollback().await.map_err(|_| RotationError::Database)?;
        return Ok(report);
    }
    if version != request.expected_version {
        return Err(RotationError::VersionConflict);
    }
    let sealed = seal_email_credential(tenant, id, &request.new_key_id, &secret, keys)
        .map_err(|_| RotationError::CredentialUnavailable)?;
    let reopened = open_email_credential(tenant, id, &sealed, keys)
        .map_err(|_| RotationError::CredentialUnavailable)?;
    if secret.expose().as_bytes() != reopened.expose().as_bytes() {
        return Err(RotationError::CredentialUnavailable);
    }
    if request.mode == RotationMode::Preview {
        report.state = RotationState::Preview;
        tx.rollback().await.map_err(|_| RotationError::Database)?;
        return Ok(report);
    }
    // Claim acquisition locks this same settings row. Do not rewrap while an
    // existing preparation/submission still owns a version-bound fence.
    let active = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT 1 AS n FROM important_email_intent WHERE tenant_id=$1 AND state IN('CLAIMED','SUBMITTING') LIMIT 1",
        [tenant.into()])).await.map_err(|_| RotationError::Database)?;
    if active.is_some() {
        return Err(RotationError::ActiveDelivery);
    }
    let updated = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE tenant_email_settings SET credential_cipher=$3,credential_nonce=$4,credential_key_id=$5,version=version+1,updated_at=clock_timestamp() WHERE tenant_id=$1 AND version=$2 RETURNING version",
        [tenant.into(), version.into(), sealed.cipher.into(), sealed.nonce.into(), sealed.key_id.into()]))
        .await.map_err(|_| RotationError::Database)?.ok_or(RotationError::VersionConflict)?;
    let next: i64 = updated
        .try_get("", "version")
        .map_err(|_| RotationError::Database)?;
    append_audit(
        &tx,
        &AuditRecordInput {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            user_id: None,
            entity_type: "tenant_email_settings".into(),
            entity_id: Some(id),
            action: "EMAIL_CREDENTIAL_ROTATED".into(),
            before_state: Some(serde_json::json!({"version":version})),
            after_state: Some(serde_json::json!({"version":next,"credential_rotated":true})),
            ip_address: None,
            user_agent: None,
            created_at: chrono::Utc::now(),
        },
    )
    .await
    .map_err(|_| RotationError::Database)?;
    tx.commit().await.map_err(|_| RotationError::Database)?;
    report.settings_version = next;
    report.changed_credentials = 1;
    report.state = RotationState::Rotated;
    Ok(report)
}

/// Reads a protected URL file without ever returning its contents in an error or report.
pub fn validate_rotation_url(url: &str, expected_database: &str) -> Result<(), RotationError> {
    let parsed = reqwest::Url::parse(url).map_err(|_| RotationError::Configuration)?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql")
        || parsed.host_str().is_none()
        || parsed.username().is_empty()
        || parsed.fragment().is_some()
        || !identifier(expected_database)
        || !expected_database.starts_with("company_")
        || parsed.path() != format!("/{expected_database}")
    {
        return Err(RotationError::Binding);
    }
    let mut parameters = std::collections::BTreeMap::new();
    for (name, value) in parsed.query_pairs() {
        if ![
            "sslmode",
            "sslrootcert",
            "sslcert",
            "sslkey",
            "connect_timeout",
            "application_name",
        ]
        .contains(&name.as_ref())
            || parameters
                .insert(name.into_owned(), value.into_owned())
                .is_some()
        {
            return Err(RotationError::Configuration);
        }
    }
    if parameters.get("sslmode").map(String::as_str) != Some("verify-full")
        || !parameters
            .get("sslrootcert")
            .is_some_and(|path| std::path::Path::new(path).is_absolute())
        || parameters.contains_key("sslcert") != parameters.contains_key("sslkey")
    {
        return Err(RotationError::Configuration);
    }
    for key in ["sslcert", "sslkey"] {
        if parameters
            .get(key)
            .is_some_and(|path| !std::path::Path::new(path).is_absolute())
        {
            return Err(RotationError::Configuration);
        }
    }
    Ok(())
}
pub async fn connect_for_rotation(
    request: &RotationRequest,
    file: &std::path::Path,
) -> Result<DatabaseConnection, RotationError> {
    request.validate()?;
    let bytes = super::keyring::read_private_file(file, 16 * 1024)
        .await
        .map_err(|_| RotationError::Configuration)?;
    let url = std::str::from_utf8(&bytes)
        .map_err(|_| RotationError::Configuration)?
        .trim();
    validate_rotation_url(url, &request.expected_database)?;
    let mut options = sea_orm::ConnectOptions::new(url.to_owned());
    options
        .max_connections(1)
        .min_connections(0)
        .connect_timeout(std::time::Duration::from_secs(5))
        .sqlx_logging(false)
        .set_schema_search_path(format!("{},pg_catalog", request.expected_schema));
    sea_orm::Database::connect(options)
        .await
        .map_err(|_| RotationError::Database)
}
