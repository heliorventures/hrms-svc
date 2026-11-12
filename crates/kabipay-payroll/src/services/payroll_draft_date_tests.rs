//! Repeated stored-review reads must retain the source command's verified date.
use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow};
use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};
#[derive(Debug)]
struct SourceOnly {
    cycle: ProxyRow,
    snapshot: Arc<Mutex<serde_json::Value>>,
    writes: Arc<Mutex<Vec<String>>>,
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for SourceOnly {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        if sql.starts_with("SELECT pg_advisory_xact_lock(") {
            return Ok(vec![]);
        }
        self.queries.lock().unwrap().push(sql.clone());
        if sql.starts_with("SELECT snapshot FROM payroll_draft_calculation") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "snapshot".into(),
                self.snapshot.lock().unwrap().clone().into(),
            )]))]);
        }
        if sql.contains("FROM \"payroll_cycle\"") {
            return Ok(vec![self.cycle.clone()]);
        }
        if sql.starts_with("SELECT COALESCE(jsonb_agg") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "value".into(),
                "[]".into(),
            )]))]);
        }
        if sql.starts_with("SELECT employee_id,revision FROM loan_employee_state") {
            return Ok(vec![]);
        }
        if sql.starts_with("SELECT COUNT(*) AS n FROM payslip") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "n".into(),
                0i64.into(),
            )]))]);
        }
        Err(DbErr::Custom(format!(
            "source-only review cannot query {sql}"
        )))
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        let sql = statement.to_string();
        assert!(!sql.contains("kabipay_ops"));
        self.writes.lock().unwrap().push(sql);
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
}
#[tokio::test]
async fn repeated_existing_review_reads_never_select_ops_with_verified_date() {
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let date = chrono::NaiveDate::from_ymd_opt(2026, 10, 19).unwrap();
    let cycle = ProxyRow::new(BTreeMap::from([
        ("id".into(), id.into()),
        ("tenant_id".into(), tenant.into()),
        ("name".into(), "Synthetic".into()),
        ("month".into(), 10i32.into()),
        ("year".into(), 2026i32.into()),
        ("status".into(), "DRAFT".into()),
        ("payment_date".into(), Some(date).into()),
        ("processed_by".into(), Option::<Uuid>::None.into()),
        (
            "processed_at".into(),
            Option::<chrono::DateTime<chrono::Utc>>::None.into(),
        ),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
    ]));
    let snapshot = serde_json::to_value(PayrollDraft {
        cycle_id: id,
        revision: 1,
        fingerprint: "synthetic".into(),
        employees: vec![],
        can_finalize: false,
        finalization_block_reason: None,
    })
    .unwrap();
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(SourceOnly {
            cycle,
            snapshot: Arc::new(Mutex::new(snapshot)),
            queries: queries.clone(),
            writes: Arc::new(Mutex::new(vec![])),
        })),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let prior = find_with_date(&db, tenant, id, date)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prior.revision, 1);
    }
    assert_eq!(queries.lock().unwrap().len(), 4);
}

#[tokio::test]
async fn public_existing_draft_read_never_loads_metadata_from_company() {
    let (db, tenant, id, _, _) = company().await;
    let context = crate::metadata_test_support::metadata(tenant, true).await;
    let draft = find(&db, tenant, &context, id).await.unwrap().unwrap();
    assert_eq!(draft.revision, 1);
    assert!(draft.can_finalize);
}
async fn company() -> (
    DatabaseConnection,
    Uuid,
    Uuid,
    Arc<Mutex<Vec<String>>>,
    Arc<Mutex<serde_json::Value>>,
) {
    let tenant = Uuid::new_v4();
    let id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let date = now.date_naive();
    let cycle = ProxyRow::new(BTreeMap::from([
        ("id".into(), id.into()),
        ("tenant_id".into(), tenant.into()),
        ("name".into(), "Synthetic".into()),
        ("month".into(), 10i32.into()),
        ("year".into(), 2026i32.into()),
        ("status".into(), "DRAFT".into()),
        ("payment_date".into(), Some(date).into()),
        ("processed_by".into(), Option::<Uuid>::None.into()),
        (
            "processed_at".into(),
            Option::<chrono::DateTime<chrono::Utc>>::None.into(),
        ),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
    ]));
    let queries = Arc::new(Mutex::new(vec![]));
    let writes = Arc::new(Mutex::new(vec![]));
    let snapshot = Arc::new(Mutex::new(serde_json::json!({
        "cycle_id": id, "revision": 1, "fingerprint": "initial", "employees": [{"employee_id":Uuid::new_v4(), "employee_label":"Synthetic", "outcome":"READY", "reason":null, "prepared":null}], "can_finalize": false
    })));
    let proxy = SourceOnly {
        cycle,
        snapshot: snapshot.clone(),
        queries: queries.clone(),
        writes: writes.clone(),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy)))
        .await
        .unwrap();
    (db, tenant, id, writes, snapshot)
}
#[tokio::test]
async fn payment_date_edit_never_loads_metadata_from_company() {
    let (db, tenant, id, writes, snapshot) = company().await;
    let context = crate::metadata_test_support::metadata(tenant, true).await;
    let tx = db.begin().await.unwrap();
    let fingerprint = crate::services::loan_recovery::fingerprint(&tx, tenant, &context)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    snapshot.lock().unwrap()["fingerprint"] = fingerprint.into();
    set_payment_date(
        &db,
        tenant,
        &crate::metadata_test_support::claims(tenant),
        &context,
        id,
        context.business_date(),
        Some(1),
    )
    .await
    .unwrap();
    assert!(writes
        .lock()
        .unwrap()
        .iter()
        .any(|sql| sql.starts_with("UPDATE payroll_cycle SET payment_date=")));
}
#[tokio::test]
async fn payment_date_edit_rejects_wrong_tenant_metadata_before_company_work() {
    let (db, tenant, id, writes, _) = company().await;
    let context = crate::metadata_test_support::metadata(Uuid::new_v4(), true).await;
    assert!(set_payment_date(
        &db,
        tenant,
        &crate::metadata_test_support::claims(tenant),
        &context,
        id,
        context.business_date(),
        Some(1)
    )
    .await
    .is_err());
    assert!(
        writes.lock().unwrap().is_empty(),
        "wrong tenant opened company work"
    );
}

#[tokio::test]
async fn public_read_rejects_wrong_tenant_context_before_company_reads() {
    let tenant = Uuid::new_v4();
    let context = crate::metadata_test_support::metadata(Uuid::new_v4(), true).await;
    let error = find(
        &DatabaseConnection::Disconnected,
        tenant,
        &context,
        Uuid::new_v4(),
    )
    .await
    .unwrap_err();
    assert!(matches!(error, KabiPayError::Forbidden(_)));
}
