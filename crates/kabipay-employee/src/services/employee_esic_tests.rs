use super::employee_esic_service::{normalize_esic, set};
use kabipay_common::KabiPayError;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[test]
fn esic_keeps_leading_zeroes_and_trims_outer_whitespace() {
    assert_eq!(
        normalize_esic(" 0123456789 ").unwrap(),
        Some("0123456789".into())
    );
}

#[test]
fn blank_esic_explicitly_clears_the_optional_value() {
    assert_eq!(normalize_esic("   ").unwrap(), None);
}

#[test]
fn esic_rejects_non_ascii_digits_and_incorrect_lengths() {
    for value in [
        "123456789",
        "12345678901",
        "123456789A",
        "12345 6789",
        "１２３４５６７８９０",
    ] {
        assert!(normalize_esic(value).is_err());
    }
}

#[derive(Debug)]
struct EsicWriteProxy {
    statements: Arc<Mutex<Vec<String>>>,
    affected: u64,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for EsicWriteProxy {
    async fn query(&self, _statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        Err(DbErr::Custom("unexpected query".into()))
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.statements.lock().unwrap().push(statement.to_string());
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: self.affected,
        })
    }
}

async fn connection(affected: u64) -> (sea_orm::DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(EsicWriteProxy {
            statements: Arc::clone(&statements),
            affected,
        })),
    )
    .await
    .unwrap();
    (db, statements)
}

#[tokio::test]
async fn esic_write_targets_only_the_active_employee_in_the_tenant() {
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let (db, statements) = connection(1).await;
    assert_eq!(
        set(&db, tenant, employee, "0123456789").await.unwrap(),
        Some("0123456789".into())
    );
    let statements = statements.lock().unwrap();
    assert_eq!(statements.len(), 1);
    let sql = &statements[0];
    assert!(sql.contains(&tenant.to_string()));
    assert!(sql.contains(&employee.to_string()));
    assert!(sql.to_ascii_uppercase().contains("\"IS_DELETED\" = FALSE"));
    assert!(sql.contains("\"esic_number\" = '0123456789'"));
    assert!(!sql.contains("\"uan_number\""));
}

#[tokio::test]
async fn esic_validation_happens_before_writing_and_missing_employee_is_rejected() {
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let (db, statements) = connection(0).await;
    assert!(set(&db, tenant, employee, "123").await.is_err());
    assert!(statements.lock().unwrap().is_empty());
    assert!(matches!(
        set(&db, tenant, employee, "").await,
        Err(KabiPayError::NotFound { .. })
    ));
    assert!(statements.lock().unwrap()[0].contains("\"esic_number\" = NULL"));
}
