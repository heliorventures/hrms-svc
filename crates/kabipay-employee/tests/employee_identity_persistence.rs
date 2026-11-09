use chrono::{NaiveDate, Utc};
use kabipay_employee::entities::d0007_employee_core::employee;
use kabipay_employee::services::employee_service::{update, update_in_transaction, EmployeePatch};
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DatabaseConnection, DbBackend, DbErr, Iden, Iterable, ModelTrait, ProxyDatabaseTrait,
    ProxyExecResult, ProxyRow, Statement, TransactionTrait,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

fn employee_row(tenant_id: Uuid, id: Uuid) -> employee::Model {
    employee::Model {
        id,
        tenant_id,
        user_id: None,
        department_id: None,
        designation_id: None,
        cost_center_id: None,
        location_id: None,
        reporting_manager_id: None,
        employee_code: "EMP-001".into(),
        first_name: "Identity".into(),
        last_name: "Test".into(),
        date_of_birth: None,
        gender: None,
        marital_status: None,
        blood_group: None,
        nationality: None,
        employment_type: None,
        status: "ACTIVE".into(),
        date_of_joining: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
        probation_end_date: None,
        notice_period_days: None,
        emergency_contact_name: None,
        emergency_contact_phone: None,
        emergency_contact_relation: None,
        personal_phone: None,
        current_address: None,
        permanent_address: None,
        uan_number: None,
        esic_number: None,
        is_deleted: false,
        deleted_at: None,
        deleted_by: None,
        created_at: Utc::now(),
        updated_at: Utc::now(),
    }
}

#[derive(Debug)]
struct IdentityProxy {
    row: employee::Model,
    statements: Arc<Mutex<Vec<String>>>,
    missing: bool,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for IdentityProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(statement.to_string());
        if self.missing {
            return Ok(Vec::new());
        }
        let fields: BTreeMap<_, _> = employee::Column::iter()
            .map(|column| (column.to_string(), self.row.get(column)))
            .collect();
        Ok(vec![ProxyRow::new(fields)])
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.statements.lock().unwrap().push(statement.to_string());
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
}

async fn connection(
    row: employee::Model,
    missing: bool,
) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(IdentityProxy {
            row,
            statements: Arc::clone(&statements),
            missing,
        })),
    )
    .await
    .unwrap();
    (db, statements)
}

#[tokio::test]
async fn employee_identity_validates_code_before_opening_a_transaction() {
    for code in ["".to_string(), "   ".into(), "X".repeat(51)] {
        let error = update(
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            Uuid::new_v4(),
            EmployeePatch {
                employee_code: Some(code),
                ..Default::default()
            },
        )
        .await
        .expect_err("invalid employee code must be rejected before database access");
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }
}

#[tokio::test]
async fn employee_identity_writes_code_and_joining_date_using_the_existing_employee_id() {
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let (db, statements) = connection(employee_row(tenant, id), false).await;
    update(
        &db,
        tenant,
        id,
        EmployeePatch {
            employee_code: Some(" EMP-002 ".into()),
            date_of_joining: NaiveDate::from_ymd_opt(2026, 2, 15),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let statements = statements.lock().unwrap();
    let sql = statements
        .iter()
        .find(|sql| sql.starts_with("UPDATE \"employee\""))
        .unwrap();
    assert!(sql.contains("\"employee_code\" = 'EMP-002'"), "{sql}");
    assert!(sql.contains("\"date_of_joining\" = '2026-02-15'"), "{sql}");
    assert!(sql.contains(&id.to_string()), "{sql}");
    let assignments = sql.split(" WHERE ").next().unwrap();
    assert!(!assignments.contains("\"id\" ="), "{sql}");
    let lookup = statements
        .iter()
        .find(|sql| sql.contains("FROM \"employee\""))
        .unwrap();
    assert!(lookup.contains(&tenant.to_string()), "{lookup}");
    assert!(
        lookup
            .to_ascii_lowercase()
            .contains("\"is_deleted\" = false"),
        "{lookup}"
    );
    assert!(statements
        .iter()
        .any(|sql| sql.contains("pg_advisory_xact_lock")));
}

#[tokio::test]
async fn employee_identity_leaves_omitted_corrections_unchanged() {
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let (db, statements) = connection(employee_row(tenant, id), false).await;
    update(
        &db,
        tenant,
        id,
        EmployeePatch {
            first_name: Some("Corrected".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    let statements = statements.lock().unwrap();
    let sql = statements
        .iter()
        .find(|sql| sql.starts_with("UPDATE \"employee\""))
        .unwrap();
    assert!(!sql.contains("\"employee_code\" ="), "{sql}");
    assert!(!sql.contains("\"date_of_joining\" ="), "{sql}");
}

#[tokio::test]
async fn employee_identity_transaction_entry_also_rejects_invalid_codes() {
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let (db, statements) = connection(employee_row(tenant, id), false).await;
    let transaction = db.begin().await.unwrap();
    let error = update_in_transaction(
        &transaction,
        tenant,
        id,
        EmployeePatch {
            employee_code: Some("   ".into()),
            ..Default::default()
        },
        &[],
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "VALIDATION_ERROR");
    assert!(statements.lock().unwrap().is_empty());
}

#[tokio::test]
async fn employee_identity_rejects_an_employee_missing_from_the_tenant() {
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let (db, statements) = connection(employee_row(tenant, id), true).await;
    let error = update(
        &db,
        tenant,
        id,
        EmployeePatch {
            employee_code: Some("EMP-002".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap_err();
    assert_eq!(error.code(), "NOT_FOUND");
    let statements = statements.lock().unwrap();
    assert!(!statements.iter().any(|sql| sql.starts_with("UPDATE")));
    assert!(statements
        .iter()
        .any(|sql| sql.contains(&tenant.to_string())));
}
