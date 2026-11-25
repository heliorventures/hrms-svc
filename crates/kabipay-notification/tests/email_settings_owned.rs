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
struct OwnedFixture {
    port: u16,
    tenant: Uuid,
    actors: Vec<Uuid>,
    system_id: String,
    data_directory: PathBuf,
}
fn fixture() -> OwnedFixture {
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
async fn connect(fixture: &OwnedFixture, user: &str) -> DatabaseConnection {
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
async fn query(db: &DatabaseConnection, sql: &str) -> QueryResult {
    db.query_one(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap()
        .unwrap()
}
fn input(credential: SecretChange, enabled: bool, leave: bool) -> EmailSettingsInput {
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
fn keys(tenant: Uuid) -> Keyring {
    let mut document = serde_json::json!({"version":1,"tenants":{}});
    document["tenants"][tenant.to_string()] = serde_json::json!({"active_key_id":"v1",
        "keys":{"v1":"AQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQEBAQE="}});
    Keyring::from_json(&serde_json::to_vec(&document).unwrap()).unwrap()
}

#[tokio::test]
#[ignore = "run only through the owned local PostgreSQL fixture"]
async fn owned_settings_transactions() {
    let fixture = fixture();
    let owner = connect(&fixture, "fixture_owner").await;
    let system: String = query(
        &owner,
        "SELECT system_identifier::text AS id FROM pg_control_system()",
    )
    .await
    .try_get("", "id")
    .unwrap();
    assert_eq!(system, fixture.system_id);
    let db = connect(&fixture, "hrms_company_notification").await;
    let user: String = query(&db, "SELECT current_user::text AS name")
        .await
        .try_get("", "name")
        .unwrap();
    assert_eq!(user, "hrms_company_notification");
    let tenant = fixture.tenant;
    let actor = fixture.actors[0];
    let initial = load_email_settings(&db, tenant).await.unwrap();
    assert_eq!(initial.version, 1);
    assert!(!initial.enabled && !initial.credential_present);
    assert!(load_email_settings(&db, Uuid::new_v4()).await.is_err());
    let configured = save_email_settings(
        &db,
        tenant,
        actor,
        1,
        input(SecretChange::Preserve, false, false),
        None,
    )
    .await
    .unwrap();
    assert_eq!(configured.version, 2);
    let keys = keys(tenant);
    let secret = || {
        SecretChange::Replace(
            SecretString::new("synthetic-password-for-local-test".into()).unwrap(),
        )
    };
    let replaced = save_email_settings(
        &db,
        tenant,
        actor,
        2,
        input(secret(), false, false),
        Some(&keys),
    )
    .await
    .unwrap();
    assert!(replaced.credential_present && !replaced.enabled);
    let cipher: Vec<u8> = query(
        &owner,
        "SELECT credential_cipher FROM tenant_email_settings",
    )
    .await
    .try_get("", "credential_cipher")
    .unwrap();
    assert!(!cipher.windows(9).any(|bytes| bytes == b"synthetic"));
    let enabled = save_email_settings(
        &db,
        tenant,
        actor,
        3,
        input(SecretChange::Preserve, true, true),
        Some(&keys),
    )
    .await
    .unwrap();
    assert_eq!(enabled.version, 4);
    let first = query(
        &owner,
        "SELECT enabled_after,category_cutoffs FROM tenant_email_settings",
    )
    .await;
    let first_enabled: DateTime<Utc> = first.try_get("", "enabled_after").unwrap();
    let first_cutoffs: serde_json::Value = first.try_get("", "category_cutoffs").unwrap();
    assert!(first_cutoffs.get("leave").is_some());
    let disabled = save_email_settings(
        &db,
        tenant,
        actor,
        4,
        input(SecretChange::Preserve, false, true),
        None,
    )
    .await
    .unwrap();
    assert_eq!(disabled.version, 5);
    let still_cipher: Vec<u8> = query(
        &owner,
        "SELECT credential_cipher FROM tenant_email_settings",
    )
    .await
    .try_get("", "credential_cipher")
    .unwrap();
    assert_eq!(
        cipher, still_cipher,
        "preserve does not rewrite the encrypted secret"
    );
    let resumed = save_email_settings(
        &db,
        tenant,
        actor,
        5,
        input(SecretChange::Preserve, true, true),
        Some(&keys),
    )
    .await
    .unwrap();
    assert_eq!(resumed.version, 6);
    let second = query(&owner, "SELECT enabled_after FROM tenant_email_settings").await;
    let second_enabled: DateTime<Utc> = second.try_get("", "enabled_after").unwrap();
    assert!(
        second_enabled > first_enabled,
        "reenable has a new database-time cutoff"
    );
    let mut category = input(SecretChange::Preserve, true, true);
    category.categories.approval = true;
    let category = save_email_settings(&db, tenant, actor, 6, category, Some(&keys))
        .await
        .unwrap();
    assert_eq!(category.version, 7);
    let cutoffs = query(
        &owner,
        "SELECT enabled_after,category_cutoffs FROM tenant_email_settings",
    )
    .await;
    let enabled_after: DateTime<Utc> = cutoffs.try_get("", "enabled_after").unwrap();
    let category_cutoffs: serde_json::Value = cutoffs.try_get("", "category_cutoffs").unwrap();
    assert_eq!(enabled_after, second_enabled);
    assert!(category_cutoffs.get("approval").is_some());
    assert_eq!(category_cutoffs["leave"], first_cutoffs["leave"]);
    let cleared = save_email_settings(
        &db,
        tenant,
        actor,
        7,
        input(SecretChange::Clear, true, true),
        None,
    )
    .await
    .unwrap();
    assert_eq!(cleared.version, 8);
    assert!(!cleared.enabled && !cleared.credential_present);
    assert!(
        save_email_settings(&db, tenant, actor, 8, input(secret(), false, false), None)
            .await
            .is_err()
    );
    let ready = save_email_settings(
        &db,
        tenant,
        actor,
        8,
        input(secret(), false, false),
        Some(&keys),
    )
    .await
    .unwrap();
    assert_eq!(ready.version, 9);
    assert!(save_email_settings(
        &db,
        tenant,
        actor,
        9,
        input(SecretChange::Preserve, true, true),
        None
    )
    .await
    .is_err());

    // Audit failure rolls back the settings update, not just the response.
    let original = std::env::var_os("KABIPAY_PRIVATE_EVENT_KEYS_FILE").unwrap();
    std::env::set_var(
        "KABIPAY_PRIVATE_EVENT_KEYS_FILE",
        fixture.data_directory.join("missing-synthetic-key"),
    );
    let failed = save_email_settings(
        &db,
        tenant,
        actor,
        9,
        input(SecretChange::Preserve, false, true),
        None,
    )
    .await;
    std::env::set_var("KABIPAY_PRIVATE_EVENT_KEYS_FILE", &original);
    assert!(failed.is_err());
    assert_eq!(load_email_settings(&db, tenant).await.unwrap().version, 9);

    let mut tests = Vec::new();
    for (index, actor) in fixture.actors.iter().enumerate() {
        let count = if index < 3 { 3 } else { 1 };
        for _ in 0..count {
            tests.push((*actor, queue_test(&db, tenant, *actor, 9).await.unwrap()));
        }
        let denied = queue_test(&db, tenant, *actor, 9).await;
        assert!(matches!(
            denied,
            Err(KabiPayError::BusinessRule {
                code: "EMAIL_TEST_RATE_LIMITED",
                ..
            })
        ));
    }
    let first_test = &tests[0].1;
    assert_eq!(first_test.state, EmailTestState::Queued);
    assert!(
        load_test_status(&db, tenant, fixture.actors[1], first_test.id)
            .await
            .is_err()
    );
    assert!(load_test_status(&db, Uuid::new_v4(), actor, first_test.id)
        .await
        .is_err());
    assert_eq!(
        load_test_status(&db, tenant, actor, first_test.id)
            .await
            .unwrap()
            .state,
        EmailTestState::Queued
    );

    // Decrypt only synthetic audit facts to prove no sender or credential values
    // were copied into the audit payload. Service SQL never reads this outbox.
    let audit_keys = AuditKeys::from_json(&std::fs::read(original).unwrap()).unwrap();
    let frames = owner
        .query_all(Statement::from_string(
            DbBackend::Postgres,
            "SELECT event_envelope FROM company_event_outbox".to_owned(),
        ))
        .await
        .unwrap();
    assert_eq!(
        frames.len(),
        8,
        "only successful settings changes commit audit records"
    );
    for frame in frames {
        let envelope: Envelope =
            serde_json::from_value(frame.try_get("", "event_envelope").unwrap()).unwrap();
        let bytes = audit_keys
            .decrypt_frames(&[envelope], tenant, "audit")
            .unwrap();
        let text = String::from_utf8(bytes).unwrap();
        assert!(
            !text.contains("synthetic-password")
                && !text.contains("@example.com")
                && !text.contains("smtp.example.com")
        );
    }
    db.close().await.unwrap();
    owner.close().await.unwrap();
}
