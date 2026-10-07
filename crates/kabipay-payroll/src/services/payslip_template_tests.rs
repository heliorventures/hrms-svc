//! Persistence and tenant-boundary checks without a live company database.
use super::{payroll_service::upsert_payroll_compliance_setting, payslip_template};
use chrono::Utc;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug)]
struct SettingsProxy {
    responses: Mutex<VecDeque<Vec<ProxyRow>>>,
    queries: Arc<Mutex<Vec<String>>>,
    fail: bool,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for SettingsProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.queries.lock().unwrap().push(statement.to_string());
        if self.fail {
            return Err(DbErr::Custom("settings unavailable".into()));
        }
        self.responses
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| DbErr::Custom("unexpected query".into()))
    }
    async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom("unexpected non-returning write".into()))
    }
}

fn row(tenant: Uuid, template: &str) -> ProxyRow {
    row_with_fields(tenant, template, super::payslip_employee_fields::defaults())
}

pub(super) fn row_with_fields(tenant: Uuid, template: &str, fields: Vec<String>) -> ProxyRow {
    let now = Utc::now();
    ProxyRow::new(BTreeMap::from([
        ("id".into(), Uuid::new_v4().into()),
        ("tenant_id".into(), tenant.into()),
        ("employer_tan".into(), Option::<String>::None.into()),
        ("employer_legal_name".into(), Option::<String>::None.into()),
        ("base_salary_component_code".into(), "BASIC".into()),
        ("arrear_salary_component_code".into(), "ARREAR".into()),
        ("payslip_header_title".into(), Option::<String>::None.into()),
        (
            "payslip_logo_file_storage_id".into(),
            Option::<Uuid>::None.into(),
        ),
        ("payslip_template".into(), template.into()),
        (
            "payslip_employee_fields".into(),
            serde_json::json!(fields).into(),
        ),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
    ]))
}

pub(super) async fn connection(
    responses: Vec<Vec<ProxyRow>>,
    fail: bool,
) -> (sea_orm::DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let queries = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(SettingsProxy {
            responses: Mutex::new(responses.into()),
            queries: Arc::clone(&queries),
            fail,
        })),
    )
    .await
    .unwrap();
    (db, queries)
}

#[tokio::test]
async fn payslip_template_reads_only_the_requested_tenant() {
    let tenant = Uuid::new_v4();
    let (db, queries) = connection(vec![vec![row(tenant, "TABLE")]], false).await;
    assert_eq!(payslip_template::load(&db, tenant).await.unwrap(), "TABLE");
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].contains(&format!("\"tenant_id\" = '{}'", tenant)));
}

#[tokio::test]
async fn payslip_template_absence_defaults_but_read_failure_does_not() {
    let (db, _) = connection(vec![vec![]], false).await;
    assert_eq!(
        payslip_template::load(&db, Uuid::new_v4()).await.unwrap(),
        "EXISTING"
    );
    let (db, _) = connection(vec![], true).await;
    assert!(payslip_template::load(&db, Uuid::new_v4()).await.is_err());
}

#[tokio::test]
async fn payslip_template_omitted_update_does_not_overwrite_saved_selection() {
    let tenant = Uuid::new_v4();
    let saved = row(tenant, "TABLE");
    let (db, queries) = connection(vec![vec![saved.clone()], vec![saved]], false).await;
    let result = upsert_payroll_compliance_setting(
        &db, tenant, None, None, None, None, None, None, None, None,
    )
    .await
    .unwrap();
    assert_eq!(result.payslip_template, "TABLE");
    let queries = queries.lock().unwrap();
    let update = queries
        .iter()
        .find(|query| query.starts_with("UPDATE"))
        .unwrap();
    let assignments = update.split(" RETURNING ").next().unwrap();
    assert!(!assignments.contains("payslip_template"), "{update}");
    assert!(!assignments.contains("payslip_employee_fields"), "{update}");
}

#[tokio::test]
async fn employee_fields_explicit_empty_selection_is_persisted() {
    let tenant = Uuid::new_v4();
    let (db, queries) = connection(
        vec![
            vec![row(tenant, "TABLE")],
            vec![row_with_fields(tenant, "TABLE", vec![])],
        ],
        false,
    )
    .await;
    let result = upsert_payroll_compliance_setting(
        &db,
        tenant,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(vec![]),
    )
    .await
    .unwrap();
    assert_eq!(result.payslip_employee_fields, serde_json::json!([]));
    assert_eq!(result.payslip_template, "TABLE");
    assert!(queries
        .lock()
        .unwrap()
        .iter()
        .any(|query| query.contains("\"payslip_employee_fields\" = '[]'")));
}

#[tokio::test]
async fn invalid_employee_field_selection_is_rejected_before_database_access() {
    let tenant = Uuid::new_v4();
    let (db, queries) = connection(vec![], false).await;
    let result = upsert_payroll_compliance_setting(
        &db,
        tenant,
        None,
        None,
        None,
        None,
        None,
        None,
        None,
        Some(vec!["BANK_ACCOUNT".into()]),
    )
    .await;
    assert!(result.is_err());
    assert!(queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn payslip_template_explicit_update_is_persisted() {
    let tenant = Uuid::new_v4();
    let saved = row(tenant, "EXISTING");
    let (db, queries) = connection(vec![vec![saved], vec![row(tenant, "TABLE")]], false).await;
    let result = upsert_payroll_compliance_setting(
        &db,
        tenant,
        None,
        None,
        None,
        None,
        None,
        None,
        Some("TABLE".into()),
        None,
    )
    .await
    .unwrap();
    assert_eq!(result.payslip_template, "TABLE");
    let queries = queries.lock().unwrap();
    assert!(queries
        .iter()
        .any(|query| query.contains("\"payslip_template\" = 'TABLE'")));
}
