use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, TransactionTrait};
use std::sync::{Arc, Mutex};

#[derive(Debug)]
struct PreparationFailureProxy {
    statements: Arc<Mutex<Vec<Statement>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for PreparationFailureProxy {
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.statements.lock().unwrap().push(statement.clone());
        if statement.sql == "SELECT hrms_lock_payroll_period_input_v1($1)"
            || statement.sql.starts_with("SELECT pg_advisory_xact_lock(") {
            Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
        } else {
            Err(DbErr::Custom("unexpected write after preparation failure".into()))
        }
    }

    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(statement.clone());
        if statement.sql.contains("FROM \"payroll_cycle\"") {
            Ok(vec![])
        } else if statement.sql.contains("FROM \"payroll_period_input\"") {
            Err(DbErr::Custom("secondary query masked initial failure".into()))
        } else {
            Err(DbErr::Custom("initial configuration database failure".into()))
        }
    }
}

#[tokio::test]
async fn monthly_preparation_database_failure_escapes_without_followup_reads_or_writes() {
    let statements = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(DbBackend::Postgres,
        Arc::new(Box::new(PreparationFailureProxy { statements: statements.clone() }))).await.unwrap();
    let input = super::super::automatic_period::new_input(2026, 10).unwrap();
    let tx = db.begin().await.unwrap();
    let error = save_with_actor(&tx, Uuid::new_v4(), Some(Uuid::new_v4()), None,
        Uuid::new_v4(), input, None, None).await.unwrap_err();
    assert!(matches!(error, KabiPayError::Database(DbErr::Custom(ref message))
        if message == "initial configuration database failure"));
    let statements = statements.lock().unwrap();
    assert_eq!(statements.len(), 4);
    assert!(!statements.iter().any(|s| s.sql.contains("FROM \"payroll_period_input\"")));
}
