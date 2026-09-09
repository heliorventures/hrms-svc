//! D1 regressions execute the real ledger finalizer through an offline SQL proxy.
use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
use std::collections::{BTreeMap, VecDeque};
use std::sync::{Arc, Mutex};

#[derive(Clone, Debug)]
struct LedgerProxy {
    replies: Arc<Mutex<VecDeque<Vec<ProxyRow>>>>,
    statements: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for LedgerProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(statement.to_string());
        self.replies.lock().unwrap().pop_front()
            .ok_or_else(|| DbErr::Custom("unexpected ledger query".into()))
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom(format!("unexpected ledger execute: {statement}")))
    }
}

fn date(month: u32, day: u32) -> NaiveDate {
    NaiveDate::from_ymd_opt(2026, month, day).unwrap()
}

async fn decide_reserved_credit(approve: bool) -> Vec<String> {
    let tenant = Uuid::new_v4();
    let request = Uuid::new_v4();
    let credit = Uuid::new_v4();
    let now = Utc::now();
    let allocation = ProxyRow::new(BTreeMap::from([
        ("id".into(), Uuid::new_v4().into()),
        ("tenant_id".into(), tenant.into()),
        ("credit_id".into(), credit.into()),
        ("leave_request_id".into(), request.into()),
        ("leave_date".into(), date(10, 19).into()),
        ("units".into(), Decimal::ONE.into()),
        ("status".into(), "RESERVED".into()),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
    ]));
    let credit_row = ProxyRow::new(BTreeMap::from([
        ("id".into(), credit.into()),
        ("tenant_id".into(), tenant.into()),
        ("employee_id".into(), Uuid::new_v4().into()),
        ("claim_id".into(), Uuid::new_v4().into()),
        ("earned_units".into(), Decimal::ONE.into()),
        ("reserved_units".into(), Decimal::ONE.into()),
        ("used_units".into(), Decimal::ZERO.into()),
        ("approved_at".into(), now.into()),
        ("approval_business_date".into(), date(9, 20).into()),
        ("expires_at".into(), date(10, 20).into()),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
    ]));
    // UPDATE ... RETURNING replies permit ORM decoding; assertions below inspect
    // the actual emitted writes, not these scripted returned values.
    let proxy = LedgerProxy {
        replies: Arc::new(Mutex::new(VecDeque::from([
            vec![allocation.clone()], vec![credit_row.clone()],
            vec![credit_row], vec![allocation],
        ]))),
        statements: Arc::new(Mutex::new(Vec::new())),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy.clone())))
        .await.unwrap();
    finalize_leave_allocations(&db, tenant, request, date(10, 22), approve)
        .await.expect("a delayed decision must settle the existing reservation");
    assert!(proxy.replies.lock().unwrap().is_empty());
    let statements = proxy.statements.lock().unwrap().clone();
    assert_eq!(statements.len(), 4);
    assert!(statements[0].contains(&tenant.to_string()));
    assert!(statements[0].contains(&request.to_string()));
    assert!(statements[0].contains("'RESERVED'"));
    assert!(statements[0].ends_with("FOR UPDATE"));
    assert!(statements[1].ends_with("FOR UPDATE"));
    assert!(statements[2].starts_with("UPDATE \"comp_off_credit\" SET"));
    assert!(statements[2].contains("\"reserved_units\" = 0"));
    assert!(!statements[2].split(" RETURNING ").next().unwrap().contains("\"expires_at\" ="));
    assert!(statements[3].starts_with("UPDATE \"comp_off_allocation\" SET"));
    assert!(statements.iter().all(|sql| !sql.contains("\"leave_balance\"")));
    statements
}

#[tokio::test]
async fn delayed_approval_after_expiry_consumes_only_reserved_comp_off_credit() {
    let statements = decide_reserved_credit(true).await;
    assert!(statements[2].contains("\"used_units\" = 1"));
    assert!(statements[3].contains("\"status\" = 'USED'"));
}

#[tokio::test]
async fn delayed_rejection_expires_reservation_without_restoring_spendable_credit() {
    let statements = decide_reserved_credit(false).await;
    assert!(!statements[2].split(" RETURNING ").next().unwrap().contains("\"used_units\" ="));
    assert!(statements[3].contains("\"status\" = 'EXPIRED'"));
}

#[test]
fn october_twentieth_expiry_covers_nineteenth_but_excludes_twentieth() {
    let credit = CreditAvailability {
        id: Uuid::new_v4(), expires_at: date(10, 20), available: Decimal::ONE,
    };
    assert!(plan_allocations(&[credit.clone()], &[(date(10, 19), Decimal::ONE)]).is_ok());
    assert!(plan_allocations(&[credit], &[(date(10, 20), Decimal::ONE)]).is_err());
}
