use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
use kabipay_notification::email::{
    api::{require_email_authority, EmailSettingsAction},
    keyring::SecretString,
    settings::{EmailCategories, EmailSettingsInput, SecretChange},
};
use uuid::Uuid;

use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Clone, Debug)]
struct SettingsRows {
    statements: Arc<Mutex<Vec<String>>>,
    row: ProxyRow,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for SettingsRows {
    async fn query(&self, sql: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(sql.sql);
        Ok(vec![self.row.clone()])
    }
    async fn execute(&self, sql: Statement) -> Result<ProxyExecResult, DbErr> {
        self.statements.lock().unwrap().push(sql.sql);
        Err(DbErr::Custom("unexpected write".into()))
    }
}

async fn settings_db(tenant: Uuid) -> (DatabaseConnection, SettingsRows) {
    let row = ProxyRow::new(BTreeMap::from([
        ("id".into(), Uuid::new_v4().into()), ("tenant_id".into(), tenant.into()),
        ("version".into(), 2_i64.into()), ("enabled".into(), false.into()),
        ("smtp_host".into(), "smtp.example.com".into()), ("smtp_port".into(), 465_i32.into()),
        ("tls_mode".into(), "implicit".into()), ("sender_name".into(), "Example company".into()),
        ("sender_email".into(), "sender@example.com".into()),
        ("account_email".into(), "account@example.com".into()),
        ("categories".into(), serde_json::json!({"leave":false,"approval":false,"payslip":false,"missed_punch_out":false}).into()),
        ("category_cutoffs".into(), serde_json::json!({}).into()),
        ("enabled_after".into(), Option::<chrono::DateTime<chrono::Utc>>::None.into()),
        ("credential_present".into(), false.into()),
        ("credential_cipher".into(), Option::<Vec<u8>>::None.into()),
        ("credential_nonce".into(), Option::<Vec<u8>>::None.into()),
        ("credential_key_id".into(), Option::<String>::None.into()),
    ]));
    let proxy = SettingsRows {
        statements: Arc::new(Mutex::new(vec![])),
        row,
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy.clone())))
        .await
        .unwrap();
    (db, proxy)
}

#[tokio::test]
async fn public_read_needs_no_key_and_never_returns_cipher_fields() {
    let tenant = Uuid::new_v4();
    let (db, proxy) = settings_db(tenant).await;
    let settings = kabipay_notification::email::settings::load_email_settings(&db, tenant)
        .await
        .unwrap();
    let public = serde_json::to_value(settings).unwrap();
    for name in [
        "credential_cipher",
        "credential_nonce",
        "credential_key_id",
        "password",
    ] {
        assert!(public.get(name).is_none());
    }
    assert_eq!(public["credential_present"], false);
    let sql = proxy.statements.lock().unwrap();
    assert_eq!(sql.len(), 1);
    assert!(!sql[0].contains("SELECT *"));
    assert!(!sql[0].contains(",credential_nonce"));
}

#[tokio::test]
async fn stale_version_refuses_before_credential_changes_or_writes() {
    let tenant = Uuid::new_v4();
    let (db, proxy) = settings_db(tenant).await;
    let result = kabipay_notification::email::settings::save_email_settings(
        &db,
        tenant,
        Uuid::new_v4(),
        1,
        input(SecretChange::Clear),
        None,
    )
    .await;
    assert!(result.is_err());
    assert!(!proxy
        .statements
        .lock()
        .unwrap()
        .iter()
        .any(|sql| sql.contains("UPDATE") && !sql.contains("FOR UPDATE")));
}

fn claims(permissions: &[&str], scope: &str) -> ClientClaims {
    ClientClaims {
        sub: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        iss: CLIENT_JWT_ISSUER.into(),
        exp: 2000,
        iat: 1000,
        email: String::new(),
        employee_id: None,
        must_change_password: false,
        roles: vec!["CUSTOM_ROLE".into()],
        permissions: permissions.iter().map(|value| (*value).into()).collect(),
        permission_scopes: permissions
            .iter()
            .map(|value| ((*value).into(), scope.into()))
            .collect(),
        resource_scopes: Default::default(),
    }
}

#[test]
fn email_settings_authority_uses_exact_permissions_and_all_scope_not_roles() {
    let read = "email_settings:read";
    let manage = "email_settings:manage";
    let test = "email_settings:test";
    assert!(require_email_authority(&claims(&[read], "ALL"), EmailSettingsAction::Read).is_ok());
    assert!(
        require_email_authority(&claims(&[read, manage], "ALL"), EmailSettingsAction::Manage)
            .is_ok()
    );
    assert!(
        require_email_authority(&claims(&[read, test], "ALL"), EmailSettingsAction::Test).is_ok()
    );
    assert!(
        require_email_authority(&claims(&[manage], "ALL"), EmailSettingsAction::Manage).is_err()
    );
    assert!(require_email_authority(&claims(&[test], "ALL"), EmailSettingsAction::Test).is_err());
    assert!(
        require_email_authority(&claims(&[read, manage], "ALL"), EmailSettingsAction::Test)
            .is_err()
    );
    for scope in ["SELF", "TEAM", "DEPARTMENT", "invalid"] {
        assert!(require_email_authority(
            &claims(&[read, manage, test], scope),
            EmailSettingsAction::Read
        )
        .is_err());
    }
    let mut admin = claims(&[], "ALL");
    admin.roles = vec!["ADMIN".into(), "HR".into()];
    assert!(require_email_authority(&admin, EmailSettingsAction::Manage).is_err());
}

fn input(secret: SecretChange) -> EmailSettingsInput {
    EmailSettingsInput {
        enabled: false,
        smtp_host: "smtp.example.com".into(),
        smtp_port: 465,
        sender_name: "Example company".into(),
        sender_email: "sender@example.com".into(),
        account_email: "account@example.com".into(),
        categories: EmailCategories::default(),
        credential: secret,
    }
}

#[test]
fn credential_preserve_replace_and_clear_are_distinct_and_redacted() {
    let preserve = input(SecretChange::Preserve);
    let clear = input(SecretChange::Clear);
    let replace = input(SecretChange::Replace(
        SecretString::new("synthetic-credential".into()).unwrap(),
    ));
    assert_eq!(preserve.credential.action(), "preserve");
    assert_eq!(clear.credential.action(), "clear");
    assert_eq!(replace.credential.action(), "replace");
    assert!(!format!("{:?}", replace.credential).contains("synthetic-credential"));
    assert!(SecretString::new("bad\r\nvalue".into()).is_err());
}

#[test]
fn transport_validation_refuses_urls_insecure_ports_and_header_injection() {
    let value = input(SecretChange::Preserve);
    assert!(value.validate().is_ok());
    for host in [
        "https://smtp.example.com",
        "example.com/path",
        "user@host",
        "example.com\nDATA",
        "localhost",
        "127.0.0.1",
        "8.8.8.8",
    ] {
        let mut value = input(SecretChange::Preserve);
        value.smtp_host = host.into();
        assert!(value.validate().is_err());
    }
    let mut value = input(SecretChange::Preserve);
    value.smtp_port = 25;
    assert!(value.validate().is_err());
    value.smtp_port = 587;
    value.sender_name = "Bad\r\nBcc: example@example.com".into();
    assert!(value.validate().is_err());
}

#[tokio::test]
async fn protected_graphql_fields_deny_before_resolving_any_database() {
    use async_graphql::{EmptySubscription, Schema};
    use kabipay_common::subgraph::TenantId;
    use kabipay_notification::resolvers::{MutationRoot, QueryRoot};
    let claims = claims(&[], "ALL");
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .data(TenantId(claims.tenant_id))
        .data(claims)
        .finish();
    let operations = [
        "{ emailSettings { version } }",
        "{ emailSettingsTest(id:\"00000000-0000-4000-8000-000000000001\") { state } }",
        "mutation { sendEmailSettingsTest(expectedVersion:1) { id state } }",
        "mutation { saveEmailSettings(input:{ expectedVersion:1,enabled:false,smtpHost:\"\",smtpPort:465,senderName:\"\",senderEmail:\"\",accountEmail:\"\",categories:{leave:false,approval:false,payslip:false,missedPunchOut:false},credentialAction:PRESERVE }) { version } }",
    ];
    for operation in operations {
        let response = schema.execute(operation).await;
        assert_eq!(response.errors.len(), 1, "{response:?}");
        assert_eq!(
            response.errors[0]
                .extensions
                .as_ref()
                .and_then(|value| value.get("code")),
            Some(&async_graphql::Value::from("FORBIDDEN"))
        );
        assert!(!response.errors[0].message.contains("TenantDbCache"));
    }
}

#[test]
fn schema_returns_no_credentials_and_test_has_no_recipient_or_body_input() {
    use async_graphql::{EmptySubscription, Schema};
    use kabipay_notification::resolvers::{MutationRoot, QueryRoot};
    let sdl = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .finish()
        .sdl();
    let public = sdl
        .split("type EmailSettingsPublic {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(public.contains("credentialPresent"));
    for name in ["credentialValue", "cipher", "nonce", "keyId", "password"] {
        assert!(!public.contains(name));
    }
    let send = sdl
        .lines()
        .find(|line| line.contains("sendEmailSettingsTest("))
        .unwrap();
    assert!(send.contains("expectedVersion"));
    assert!(!send.contains("recipient") && !send.contains("body"));
    let input = sdl
        .split("input SaveEmailSettingsInput {")
        .nth(1)
        .unwrap()
        .split('}')
        .next()
        .unwrap();
    assert!(!input.contains("tenantId"));
}

#[tokio::test]
async fn saved_write_only_password_cannot_be_redirected_to_another_server() {
    let tenant = Uuid::new_v4();
    let (db, _) = settings_db(tenant).await;
    let mut previous = kabipay_notification::email::settings::load_email_settings(&db, tenant)
        .await
        .unwrap();
    previous.credential_present = true;
    let mut changed = input(SecretChange::Preserve);
    changed.smtp_host = "other.example.com".into();
    assert!(changed.validate_credential_change(&previous).is_err());
    changed.credential = SecretChange::Clear;
    assert!(changed.validate_credential_change(&previous).is_ok());
}

#[tokio::test]
async fn credential_parse_errors_never_echo_the_submitted_value() {
    use async_graphql::{EmptySubscription, Request, Schema, Variables};
    use kabipay_notification::resolvers::{MutationRoot, QueryRoot};
    let claims = claims(&["email_settings:read", "email_settings:manage"], "ALL");
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .data(claims)
        .finish();
    let response = schema.execute(Request::new("mutation($input:SaveEmailSettingsInput!){saveEmailSettings(input:$input){version}}")
        .variables(Variables::from_json(serde_json::json!({"input": {
            "expectedVersion":1,"enabled":false,"smtpHost":"smtp.example.com","smtpPort":465,
            "senderName":"Example","senderEmail":"sender@example.com","accountEmail":"account@example.com",
            "categories":{"leave":false,"approval":false,"payslip":false,"missedPunchOut":false},
            "credentialAction":"REPLACE","credentialValue":["synthetic-secret-marker"]
        }})))).await;
    assert!(!response.errors.is_empty());
    for error in response.errors {
        assert!(
            !error.message.contains("synthetic-secret-marker"),
            "credential value was echoed"
        );
    }
}
