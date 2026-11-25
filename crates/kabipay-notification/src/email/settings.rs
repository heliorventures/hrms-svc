//! Tenant-bound safe projections and transactional credential changes.
pub use super::settings_types::{
    EmailCategories, EmailSettingsInput, EmailSettingsPublic, SecretChange,
};
use super::{
    crypto::{open_email_credential, seal_email_credential, EncryptedCredential},
    keyring::{self, Keyring},
};
use chrono::{DateTime, SecondsFormat, Utc};
use kabipay_common::{
    private_events::{append_audit, AuditRecordInput},
    KabiPayError, KabiPayResult,
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DbBackend, QueryResult, Statement, TransactionTrait,
};
use uuid::Uuid;

pub(crate) const PUBLIC_PROJECTION: &str = "id,version,enabled,smtp_host,smtp_port,tls_mode,sender_name,sender_email,account_email,categories,(credential_cipher IS NOT NULL) AS credential_present";
pub(crate) fn public_row(row: &QueryResult) -> KabiPayResult<EmailSettingsPublic> {
    Ok(EmailSettingsPublic {
        id: row.try_get("", "id")?,
        version: row.try_get("", "version")?,
        enabled: row.try_get("", "enabled")?,
        smtp_host: row.try_get("", "smtp_host")?,
        smtp_port: row.try_get("", "smtp_port")?,
        tls_mode: row.try_get("", "tls_mode")?,
        sender_name: row.try_get("", "sender_name")?,
        sender_email: row.try_get("", "sender_email")?,
        account_email: row.try_get("", "account_email")?,
        categories: serde_json::from_value(row.try_get("", "categories")?)
            .map_err(|_| configuration_unavailable())?,
        credential_present: row.try_get("", "credential_present")?,
    })
}
pub(crate) fn configuration_unavailable() -> KabiPayError {
    KabiPayError::BusinessRule {
        code: "EMAIL_SETTINGS_UNAVAILABLE",
        message: "Company email settings are not available. Contact the system administrator."
            .into(),
    }
}
pub(crate) fn version_conflict() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "EMAIL_SETTINGS_VERSION_CONFLICT",
        message: "Email settings changed. Refresh before saving or sending a test.".into(),
    }
}

pub async fn load_email_settings(
    db: &DatabaseConnection,
    tenant: Uuid,
) -> KabiPayResult<EmailSettingsPublic> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("SELECT {PUBLIC_PROJECTION} FROM tenant_email_settings WHERE tenant_id=$1"),
            [tenant.into()],
        ))
        .await?
        .ok_or_else(configuration_unavailable)?;
    public_row(&row)
}

pub(crate) fn stored_credential(row: &QueryResult) -> KabiPayResult<Option<EncryptedCredential>> {
    let cipher: Option<Vec<u8>> = row.try_get("", "credential_cipher")?;
    let nonce: Option<Vec<u8>> = row.try_get("", "credential_nonce")?;
    let key_id: Option<String> = row.try_get("", "credential_key_id")?;
    match (cipher, nonce, key_id) {
        (None, None, None) => Ok(None),
        (Some(cipher), Some(nonce), Some(key_id)) => Ok(Some(EncryptedCredential {
            cipher,
            nonce,
            key_id,
        })),
        _ => Err(configuration_unavailable()),
    }
}

pub async fn save_email_settings(
    db: &DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    expected_version: i64,
    input: EmailSettingsInput,
    keys: Option<&Keyring>,
) -> KabiPayResult<EmailSettingsPublic> {
    let input = input.normalized()?;
    let tx = db.begin().await?;
    let row = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("SELECT {PUBLIC_PROJECTION},enabled_after,category_cutoffs,credential_cipher,credential_nonce,credential_key_id FROM tenant_email_settings WHERE tenant_id=$1 FOR UPDATE"),
        [tenant.into()])).await?.ok_or_else(configuration_unavailable)?;
    let previous = public_row(&row)?;
    if expected_version < 1 || previous.version != expected_version {
        return Err(version_conflict());
    }
    input.validate_credential_change(&previous)?;
    let credential = match &input.credential {
        SecretChange::Clear => None,
        SecretChange::Preserve => stored_credential(&row)?,
        SecretChange::Replace(secret) => {
            let keys = keys.ok_or_else(keyring::unavailable)?;
            Some(seal_email_credential(
                tenant,
                previous.id,
                keys.active_id(tenant)?,
                secret,
                keys,
            )?)
        }
    };
    if input.enabled {
        let encrypted = credential.as_ref().ok_or_else(configuration_unavailable)?;
        open_email_credential(
            tenant,
            previous.id,
            encrypted,
            keys.ok_or_else(keyring::unavailable)?,
        )?;
    }
    let now: DateTime<Utc> = tx
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT clock_timestamp() AS email_now".to_owned(),
        ))
        .await?
        .ok_or_else(configuration_unavailable)?
        .try_get("", "email_now")?;
    let enabled_after = if input.enabled && !previous.enabled {
        Some(now)
    } else {
        row.try_get("", "enabled_after")?
    };
    let mut cutoffs: serde_json::Value = row.try_get("", "category_cutoffs")?;
    let map = cutoffs
        .as_object_mut()
        .ok_or_else(configuration_unavailable)?;
    for ((category, enabled), (_, old)) in input
        .categories
        .entries()
        .into_iter()
        .zip(previous.categories.entries())
    {
        if enabled && !old {
            map.insert(
                category.into(),
                now.to_rfc3339_opts(SecondsFormat::Micros, true).into(),
            );
        }
    }
    let (cipher, nonce, key_id) = match credential {
        Some(value) => (Some(value.cipher), Some(value.nonce), Some(value.key_id)),
        None => (None, None, None),
    };
    let changed = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        format!("UPDATE tenant_email_settings SET version=version+1,enabled=$3,smtp_host=$4,smtp_port=$5,tls_mode=$6,sender_name=$7,sender_email=$8,account_email=$9,categories=$10,enabled_after=$11,category_cutoffs=$12,credential_cipher=$13,credential_nonce=$14,credential_key_id=$15,updated_by=$16,updated_at=clock_timestamp() WHERE tenant_id=$1 AND version=$2 RETURNING {PUBLIC_PROJECTION}"),
        [tenant.into(), expected_version.into(), input.enabled.into(), input.smtp_host.clone().into(), input.smtp_port.into(),
            (if input.smtp_port == 465 { "implicit" } else { "starttls" }).into(), input.sender_name.clone().into(),
            input.sender_email.clone().into(), input.account_email.clone().into(), serde_json::to_value(input.categories)?.into(),
            enabled_after.into(), cutoffs.into(), cipher.into(), nonce.into(), key_id.into(), actor.into()]
    )).await?.ok_or_else(version_conflict)?;
    let saved = public_row(&changed)?;
    append_audit(&tx, &AuditRecordInput {
        id: Uuid::new_v4(), tenant_id: tenant, user_id: Some(actor), entity_type: "tenant_email_settings".into(),
        entity_id: Some(saved.id), action: "EMAIL_SETTINGS_UPDATED".into(),
        before_state: Some(serde_json::json!({"version": previous.version})),
        after_state: Some(serde_json::json!({"version": saved.version, "enabled": saved.enabled,
            "categories": saved.categories, "credential_change": input.credential.action(),
            "transport_changed": input.transport_changed(&previous),
            "sender_changed": input.sender_name != previous.sender_name || input.sender_email != previous.sender_email})),
        ip_address: None, user_agent: None, created_at: now,
    }).await?;
    tx.commit().await?;
    Ok(saved)
}
