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
    snapshot: serde_json::Value,
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for SourceOnly {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.queries.lock().unwrap().push(sql.clone());
        if sql.starts_with("SELECT snapshot FROM payroll_draft_calculation") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "snapshot".into(),
                self.snapshot.clone().into(),
            )]))]);
        }
        if sql.contains("FROM \"payroll_cycle\"") {
            return Ok(vec![self.cycle.clone()]);
        }
        Err(DbErr::Custom(format!(
            "source-only review cannot query {sql}"
        )))
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom("read cannot write".into()))
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
            snapshot,
            queries: queries.clone(),
        })),
    )
    .await
    .unwrap();
    for _ in 0..2 {
        let prior = find_with_date(&db, tenant, id, Some(date))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(prior.revision, 1);
    }
    assert_eq!(queries.lock().unwrap().len(), 4);
}
