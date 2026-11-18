use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, TransactionTrait};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

#[derive(Debug)]
struct AuthorityProxy {
    digest: Option<String>,
    statements: Arc<Mutex<Vec<Statement>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for AuthorityProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(statement.clone());
        if !statement
            .sql
            .starts_with("SELECT hrms_payroll_input_fingerprint_v1(")
        {
            return Err(DbErr::Custom(
                "raw Payroll fingerprint reads forbidden".into(),
            ));
        }
        self.digest
            .as_ref()
            .map(|digest| {
                vec![ProxyRow::new(BTreeMap::from([(
                    "fingerprint".into(),
                    digest.clone().into(),
                )]))]
            })
            .ok_or_else(|| DbErr::Custom("authority unavailable".into()))
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.statements.lock().unwrap().push(statement.clone());
        if statement.sql == "SELECT hrms_lock_payroll_inputs_v1($1)"
            || statement.sql == "SELECT hrms_lock_payroll_period_input_v1($1)"
            || statement.sql == "SET LOCAL lock_timeout='15s'"
        {
            return Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            });
        }
        Err(DbErr::Custom("unexpected raw lock/write".into()))
    }
}
#[tokio::test]
async fn payroll_snapshot_lock_binds_tenant_and_never_takes_raw_input_locks() {
    let tenant = Uuid::new_v4();
    let (db, calls) = connection(None).await;
    let tx = db.begin().await.unwrap();
    lock_inputs(&tx, tenant).await.unwrap();
    assert!(lock_inputs(&tx, Uuid::nil()).await.is_err());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 2);
    assert_eq!(calls[0].sql, "SET LOCAL lock_timeout='15s'");
    assert_eq!(calls[1].sql, "SELECT hrms_lock_payroll_inputs_v1($1)");
    assert_eq!(calls[1].values.as_ref().unwrap().0, vec![tenant.into()]);
}
async fn connection(
    digest: Option<String>,
) -> (sea_orm::DatabaseConnection, Arc<Mutex<Vec<Statement>>>) {
    let statements = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(AuthorityProxy {
            digest,
            statements: statements.clone(),
        })),
    )
    .await
    .unwrap();
    (db, statements)
}
#[tokio::test]
async fn payroll_snapshot_uses_one_tenant_bound_digest_call_without_raw_fallback() {
    let tenant = Uuid::new_v4();
    let digest = "a".repeat(64);
    let (db, calls) = connection(Some(digest.clone())).await;
    assert_eq!(
        fingerprint(&db, tenant)
            .await
            .expect("bounded digest available"),
        digest
    );
    let calls = calls.lock().unwrap();
    assert_eq!(calls.len(), 1);
    assert_eq!(
        calls[0].sql,
        "SELECT hrms_payroll_input_fingerprint_v1($1) AS fingerprint"
    );
    assert_eq!(calls[0].values.as_ref().unwrap().0, vec![tenant.into()]);
}
#[tokio::test]
async fn payroll_snapshot_rejects_nil_tenant_before_database_access() {
    let (db, calls) = connection(Some("a".repeat(64))).await;
    assert!(fingerprint(&db, Uuid::nil()).await.is_err());
    assert!(calls.lock().unwrap().is_empty());
}
#[tokio::test]
async fn payroll_snapshot_refuses_missing_or_malformed_authority_without_broad_reads() {
    for digest in [
        None,
        Some("a".repeat(63)),
        Some("A".repeat(64)),
        Some("z".repeat(64)),
    ] {
        let (db, calls) = connection(digest).await;
        assert!(fingerprint(&db, Uuid::new_v4()).await.is_err());
        assert_eq!(calls.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn monthly_input_save_uses_tenant_bound_lock_authority_without_raw_lock_fallback() {
    let tenant = Uuid::new_v4();
    let (db, calls) = connection(None).await;
    let tx = db.begin().await.unwrap();
    let input = super::super::automatic_period::new_input(2026, 10).unwrap();
    let result = super::super::payroll_period_input::save_with_actor(
        &tx, tenant, Some(Uuid::new_v4()), None, Uuid::new_v4(), input, None, None,
    ).await;
    assert!(result.is_err()); // Proxy deliberately refuses the next database operation.
    let calls = calls.lock().unwrap();
    assert_eq!(calls[0].sql, "SELECT hrms_lock_payroll_period_input_v1($1)");
    assert_eq!(calls[0].values.as_ref().unwrap().0, vec![tenant.into()]);
    assert!(!calls.iter().any(|call| call.sql.starts_with("LOCK TABLE")));
}

#[tokio::test]
async fn monthly_input_save_rejects_nil_tenant_before_database_access() {
    let (db, calls) = connection(None).await;
    let tx = db.begin().await.unwrap();
    let input = super::super::automatic_period::new_input(2026, 10).unwrap();
    assert!(super::super::payroll_period_input::save_with_actor(
        &tx, Uuid::nil(), Some(Uuid::new_v4()), None, Uuid::new_v4(), input, None, None,
    ).await.is_err());
    assert!(calls.lock().unwrap().is_empty());
}
