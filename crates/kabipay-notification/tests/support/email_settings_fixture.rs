//! Launched only by email-settings-native.test.cjs in its owned loopback cluster.
use chrono::{DateTime, Utc};
use kabipay_common::{
    private_events::crypto::{Envelope, Keyring as AuditKeys},
    KabiPayError,
};
use kabipay_notification::email::{
    keyring::{Keyring, SecretString},
    settings::{
        load_email_settings, save_email_settings, EmailCategories, EmailSettingsInput, SecretChange,
    },
    test_queue::{load_test_status, queue_test, EmailTestState},
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, QueryResult,
    Statement,
};
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedFixture {
    pub port: u16,
    pub tenant: Uuid,
    pub actors: Vec<Uuid>,
    pub system_id: String,
    pub data_directory: PathBuf,
}
pub fn fixture() -> OwnedFixture {
    let path = PathBuf::from(
        std::env::var_os("KABIPAY_EMAIL_OWNED_FIXTURE").expect("use the owned Node fixture"),
    );
    assert!(path.is_absolute() && std::fs::metadata(&path).unwrap().len() < 16384);
    let fixture: OwnedFixture = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let data = fixture.data_directory.canonicalize().unwrap();
    assert_eq!(
        data.parent().unwrap(),
        path.parent().unwrap().canonicalize().unwrap()
    );
    assert!(data
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("hrms-shared-grants-"));
    let pid = std::fs::read_to_string(data.join("postmaster.pid")).unwrap();
    let lines: Vec<_> = pid.lines().collect();
    assert_eq!(PathBuf::from(lines[1]).canonicalize().unwrap(), data);
    assert_eq!(lines[3].parse::<u16>().unwrap(), fixture.port);
    assert!(fixture.port > 1024 && fixture.port != 5432 && fixture.port != 5433);
    assert_eq!(fixture.actors.len(), 4);
    fixture
}
pub async fn connect(fixture: &OwnedFixture, user: &str) -> DatabaseConnection {
    assert!(matches!(
        user,
        "fixture_owner" | "hrms_company_notification"
    ));
    let mut options = ConnectOptions::new(format!(
        "postgres://{user}:synthetic-unused@127.0.0.1:{}/company_email_fixture?sslmode=disable",
        fixture.port
    ));
    options
        .max_connections(2)
        .min_connections(1)
        .connect_timeout(Duration::from_secs(5))
        .sqlx_logging(false)
        .set_schema_search_path("tenant_fixture,pg_catalog");
    Database::connect(options).await.unwrap()
}
pub async fn query(db: &DatabaseConnection, sql: &str) -> QueryResult {
    db.query_one(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap()
        .unwrap()
}
pub fn input(credential: SecretChange, enabled: bool, leave: bool) -> EmailSettingsInput {
    EmailSettingsInput {
        enabled,
        smtp_host: "smtp.example.com".into(),
        smtp_port: 465,
        sender_name: "Example company".into(),
        sender_email: "sender@example.com".into(),
        account_email: "account@example.com".into(),
        categories: EmailCategories {
            leave,
            ..Default::default()
        },
        credential,
    }
}
pub fn keys(tenant: Uuid) -> Keyring {
    let mut document = serde_json::json!({"version":1,"tenants":{}});
    document["tenants"][tenant.to_string()] = serde_json::json!({"active_key_id":"v1",
        "keys":{"v1":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="}});
    Keyring::from_json(&serde_json::to_vec(&document).unwrap()).unwrap()
}

