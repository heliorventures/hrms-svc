use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct LoginProxy {
    current_admin: bool,
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for LoginProxy {
    async fn begin(&self) {
        self.events.lock().unwrap().push("BEGIN".into());
    }
    async fn commit(&self) {
        self.events.lock().unwrap().push("COMMIT".into());
    }
    async fn rollback(&self) {
        self.events.lock().unwrap().push("ROLLBACK".into());
    }
    fn start_rollback(&self) {
        self.events.lock().unwrap().push("ROLLBACK".into());
    }
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.events
            .lock()
            .unwrap()
            .push(format!("QUERY {statement}"));
        if statement.sql.contains(" AS n,COALESCE") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([
                ("n".into(), 1_i64.into()),
                ("own".into(), self.current_admin.into()),
            ]))]);
        }
        Ok(vec![])
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.events
            .lock()
            .unwrap()
            .push(format!("EXECUTE {statement}"));
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
}

async fn connection(current_admin: bool) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let events = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(LoginProxy {
            current_admin,
            events: Arc::clone(&events),
        })),
    )
    .await
    .unwrap();
    (db, events)
}

async fn invoke(db: &DatabaseConnection, operation: u8, actor: Option<Uuid>) -> KabiPayResult<()> {
    let tenant = Uuid::from_u128(1);
    let account = NewLoginAccount {
        username: "login-admission-test".into(),
        email: None,
        password_hash: "test-hash".into(),
        role_ids: vec![Uuid::from_u128(2)],
    };
    match operation {
        0 => create_with_login_for_actor(
            db,
            tenant,
            NewEmployee {
                employee_code: "ADMISSION-TEST".into(),
                first_name: "Admission".into(),
                last_name: "Test".into(),
                date_of_joining: chrono::NaiveDate::from_ymd_opt(2026, 11, 13).unwrap(),
                department_id: None,
                designation_id: None,
                reporting_manager_id: None,
                employment_type: None,
                status: "ACTIVE".into(),
                user_id: None,
            },
            account,
            actor,
        )
        .await
        .map(|_| ()),
        1 => provision_login_for_actor(db, tenant, Uuid::from_u128(3), account, actor)
            .await
            .map(|_| ()),
        _ => {
            reset_linked_user_password_for_actor(
                db,
                tenant,
                Uuid::from_u128(3),
                "test-hash".into(),
                actor,
            )
            .await
        }
    }
}

async fn assert_revoked_denial(operation: u8) {
    let (db, events) = connection(false).await;
    let error = invoke(&db, operation, Some(Uuid::from_u128(9)))
        .await
        .expect_err("revoked direct actor");
    assert_eq!(error.code(), "FORBIDDEN");
    let events = events.lock().unwrap();
    assert_eq!(events.first().map(String::as_str), Some("BEGIN"));
    assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
    let queries = events
        .iter()
        .filter(|event| event.starts_with("QUERY"))
        .collect::<Vec<_>>();
    assert_eq!(
        queries.len(),
        1,
        "only fresh authority may be read: {events:?}"
    );
    assert!(queries[0].contains(" AS n,COALESCE"));
    assert!(!events.iter().any(|event| event.contains("INSERT INTO")
        || event.contains("UPDATE ")
        || event.contains("DELETE FROM")
        || event == "COMMIT"));
}

#[tokio::test]
async fn revoked_login_admin_create_is_denied_before_account_or_role_reads() {
    assert_revoked_denial(0).await;
}
#[tokio::test]
async fn revoked_login_admin_provision_is_denied_before_account_or_employee_reads() {
    assert_revoked_denial(1).await;
}
#[tokio::test]
async fn revoked_login_admin_password_reset_is_denied_before_account_or_employee_reads() {
    assert_revoked_denial(2).await;
}

#[tokio::test]
async fn current_login_admin_reaches_targets_only_after_locked_fresh_admission() {
    for operation in 0..3 {
        let (db, events) = connection(true).await;
        assert_eq!(
            invoke(&db, operation, Some(Uuid::from_u128(9)))
                .await
                .unwrap_err()
                .code(),
            "NOT_FOUND"
        );
        let events = events.lock().unwrap();
        let lock = events
            .iter()
            .position(|event| event.contains("pg_advisory_xact_lock"))
            .unwrap();
        let authority = events
            .iter()
            .position(|event| event.contains(" AS n,COALESCE"))
            .unwrap();
        let target = events
            .iter()
            .enumerate()
            .filter(|(_, event)| event.starts_with("QUERY"))
            .nth(1)
            .unwrap()
            .0;
        assert!(lock < authority && authority < target, "{events:?}");
        assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
    }
}

#[tokio::test]
async fn trusted_internal_login_variants_preserve_separate_authorization_contract() {
    for operation in 0..3 {
        let (db, events) = connection(false).await;
        assert_eq!(
            invoke(&db, operation, None).await.unwrap_err().code(),
            "NOT_FOUND"
        );
        let events = events.lock().unwrap();
        assert!(!events.iter().any(
            |event| event.contains(" AS n,COALESCE") || event.contains("pg_advisory_xact_lock")
        ));
        assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
    }
}
