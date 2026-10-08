//! Exercise the production run boundary without a live tenant database.
use super::payroll_service::run_payroll_for_cycle;
use chrono::Utc;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug)]
struct ClosedCycleProxy {
    cycle: ProxyRow,
    operations: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for ClosedCycleProxy {
    async fn begin(&self) {
        self.operations.lock().unwrap().push("BEGIN".into());
    }

    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.operations.lock().unwrap().push(statement.to_string());
        Ok(vec![self.cycle.clone()])
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.operations
            .lock()
            .unwrap()
            .push(format!("WRITE {statement}"));
        Err(DbErr::Custom("closed payroll must not write".into()))
    }
}

#[tokio::test]
async fn retired_run_rejects_every_state_without_accessing_financial_records() {
    for status in ["DRAFT", "PROCESSED", "CLOSED"] {
        let tenant_id = Uuid::new_v4();
        let cycle_id = Uuid::new_v4();
        let now = Utc::now();
        let cycle = ProxyRow::new(BTreeMap::from([
            ("id".into(), cycle_id.into()),
            ("tenant_id".into(), tenant_id.into()),
            ("name".into(), "September 2026".into()),
            ("month".into(), 9i32.into()),
            ("year".into(), 2026i32.into()),
            ("status".into(), status.into()),
            (
                "payment_date".into(),
                Option::<chrono::NaiveDate>::None.into(),
            ),
            ("processed_by".into(), Option::<Uuid>::None.into()),
            (
                "processed_at".into(),
                Option::<chrono::DateTime<chrono::Utc>>::None.into(),
            ),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]));
        let operations = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(ClosedCycleProxy {
                cycle,
                operations: Arc::clone(&operations),
            })),
        )
        .await
        .unwrap();
        let error = run_payroll_for_cycle(&db, tenant_id, cycle_id, Uuid::new_v4())
            .await
            .unwrap_err();
        assert!(
            error.to_string().contains("Direct payroll run is retired"),
            "{error}"
        );
        let log = operations.lock().unwrap();
        assert!(
            log.is_empty(),
            "retired route must not read or write payroll: {log:?}"
        );
    }
}
