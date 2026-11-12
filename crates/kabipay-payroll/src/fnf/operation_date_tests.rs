//! FNF readiness and recovery posting must use the same operation snapshot.
use super::*;
use crate::metadata_test_support::{claims, metadata};
use kabipay_common::{finance_contract::ExitSubject, KabiPayError};
use kabipay_loans::{LoanModuleError, PostRecoveriesInput, RecoveryQuote};
use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement, TransactionTrait,
};
use std::{collections::BTreeMap, sync::Arc};
use uuid::Uuid;

#[derive(Debug)]
struct Company {
    fnf: ProxyRow,
    review: ProxyRow,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Company {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        if sql.starts_with("SELECT pg_advisory_xact_lock(") {
            return Ok(vec![]);
        }
        assert!(!sql.contains("kabipay_ops"), "company queried metadata");
        if sql.contains("FROM \"fnf_settlement\"") {
            return Ok(vec![self.fnf.clone()]);
        }
        if sql.contains("FROM \"fnf_financial_review\"") {
            return Ok(vec![self.review.clone()]);
        }
        if sql.contains("FROM \"fnf_payout_source\"") {
            return Ok(vec![]);
        }
        Err(DbErr::Custom("passed the snapshot future-date gate".into()))
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        let sql = statement.to_string();
        assert!(!sql.contains("kabipay_ops"));
        assert!(
            !sql.starts_with("UPDATE "),
            "future settlement wrote financial records"
        );
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
}

async fn company(
    tenant: Uuid,
    subject: &ExitSubject,
    date: chrono::NaiveDate,
    nonempty: bool,
) -> (DatabaseConnection, RecoveryQuote) {
    let now = chrono::Utc::now();
    let fnf = Uuid::new_v4();
    let lines = if nonempty {
        vec![serde_json::json!({
            "loan_id": Uuid::new_v4(), "terms_version_id": Uuid::new_v4(), "policy_version_id": Uuid::new_v4(),
            "schedule_version_id": null, "calculator_version": "test", "account_version": 1,
            "currency": "INR", "principal_before": "1", "interest_before": "0", "accrued_interest": "0",
            "principal": "1", "interest": "0", "requested": "1", "deferred": "0"
        })]
    } else {
        vec![]
    };
    let quote: RecoveryQuote = serde_json::from_value(serde_json::json!({
        "input": { "employee_id": subject.employee_id, "source": "Fnf", "source_id": fnf,
            "source_revision": 1, "period_start": format!("{}-01", date.format("%Y-%m")),
            "value_date": date, "eligible_net": "1", "available_net": "1",
            "currency": {"code":"INR", "minorUnits":2} },
        "employee_revision": 0, "lines": lines, "total": if nonempty {"1"} else {"0"},
        "fingerprint": "reviewed", "fnf_limits": {}
    }))
    .unwrap();
    let snapshot = serde_json::json!({
        "input": { "separationId": subject.separation_id, "valueDate": date,
            "currency": {"code":"INR", "minorUnits":2}, "exitReviewId": null,
            "financialApprovalReference": "reviewed", "sources": [],
            "loanDecision": {"reason":"reviewed", "loans":[]}, "legacyReview":null },
        "separationFingerprint": "source", "loanStateFingerprint": "state",
        "exposure": {"employeeId":subject.employee_id, "asOf":date, "settlementDate":date,
            "employeeRevision":0, "loans":[], "fingerprint":"exposure"},
        "recoveryQuote": quote, "availablePayout":"0", "eligiblePayout":"0", "otherRecoveries":"0",
        "loanRecovered":"0", "employeePayout":"0", "residualLoanReceivable":"0"
    });
    let fnf_row = ProxyRow::new(BTreeMap::from([
        ("id".into(), fnf.into()),
        ("tenant_id".into(), tenant.into()),
        ("separation_id".into(), subject.separation_id.into()),
        ("status".into(), "DRAFT".into()),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
        ("processed_by".into(), Option::<Uuid>::None.into()),
        (
            "processed_at".into(),
            Option::<chrono::DateTime<chrono::Utc>>::None.into(),
        ),
        (
            "leave_encashment".into(),
            Some(rust_decimal::Decimal::ZERO).into(),
        ),
        (
            "gratuity_amount".into(),
            Some(rust_decimal::Decimal::ZERO).into(),
        ),
        (
            "bonus_payable".into(),
            Some(rust_decimal::Decimal::ZERO).into(),
        ),
        (
            "recovery_amount".into(),
            Some(rust_decimal::Decimal::ZERO).into(),
        ),
        (
            "net_payable".into(),
            Some(rust_decimal::Decimal::ZERO).into(),
        ),
    ]));
    let review = ProxyRow::new(BTreeMap::from([
        ("id".into(), Uuid::new_v4().into()),
        ("tenant_id".into(), tenant.into()),
        ("fnf_id".into(), fnf.into()),
        ("revision".into(), 1i64.into()),
        ("input_hash".into(), "reviewed".into()),
        ("snapshot".into(), snapshot.into()),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
        (
            "finalized_at".into(),
            Option::<chrono::DateTime<chrono::Utc>>::None.into(),
        ),
        ("actor_id".into(), Uuid::new_v4().into()),
        ("employee_id".into(), subject.employee_id.into()),
        ("value_date".into(), date.into()),
        ("currency".into(), "INR".into()),
        ("minor_units".into(), 2i32.into()),
    ]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Company {
            fnf: fnf_row,
            review,
        })),
    )
    .await
    .unwrap();
    (db, quote)
}

#[tokio::test]
async fn future_fnf_gate_and_posting_agree_even_if_a_second_clock_has_advanced() {
    for nonempty in [false, true] {
        let tenant = Uuid::new_v4();
        let context = metadata(tenant, true).await;
        let date = context.business_date().succ_opt().unwrap();
        let subject = ExitSubject {
            separation_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(),
            last_working_date: context.business_date(),
            status: "APPROVED".into(),
            fingerprint: "source".into(),
        };
        let claims = claims(tenant);
        let (db, quote) = company(tenant, &subject, date, nonempty).await;
        let actor = review::actor(&claims, tenant, &context).unwrap();
        let tx = db.begin().await.unwrap();
        let posted = kabipay_loans::post_fnf_recoveries(
            &tx,
            &PostRecoveriesInput {
                actor: &actor,
                quote: &quote,
            },
        )
        .await;
        assert!(matches!(posted, Err(LoanModuleError::InvalidCommand)));
        drop(tx);
        let error = finalize::finalize(&db, tenant, &claims, &context, &subject, 1, "reviewed")
            .await
            .unwrap_err();
        assert!(
            matches!(error, KabiPayError::Validation(ref message) if message.starts_with("FNF can be finalized on or after")),
            "readiness disagreed with recovery posting: {error:?}"
        );
    }
}

#[derive(Debug)]
struct EmptyCompany {
    employee: Uuid,
    writes: Arc<std::sync::Mutex<Vec<Statement>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for EmptyCompany {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        assert!(!sql.contains("kabipay_ops"));
        if sql.starts_with("SELECT id FROM employee WHERE") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "id".into(),
                self.employee.into(),
            )]))]);
        }
        Ok(vec![])
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        let sql = statement.to_string();
        assert!(!sql.contains("kabipay_ops"));
        self.writes.lock().unwrap().push(statement);
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
}
#[tokio::test]
async fn empty_review_uses_one_metadata_snapshot_for_default_and_loan_exposure() {
    let tenant = Uuid::new_v4();
    let context = metadata(tenant, true).await;
    let subject = ExitSubject {
        separation_id: Uuid::new_v4(),
        employee_id: Uuid::new_v4(),
        last_working_date: context.business_date().pred_opt().unwrap(),
        status: "APPROVED".into(),
        fingerprint: "a".repeat(64),
    };
    let writes = Arc::new(std::sync::Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(EmptyCompany {
            employee: subject.employee_id,
            writes: writes.clone(),
        })),
    )
    .await
    .unwrap();
    // This control plane serves exactly one snapshot; any second clock read fails.
    let ops = crate::metadata_test_support::ops(tenant, true).await;
    let result = empty_review::prepare(&db, &ops, tenant, &claims(tenant), &subject)
        .await
        .unwrap();
    assert_eq!(result["separationId"], subject.separation_id.to_string());
    let writes = writes.lock().unwrap();
    let saved = writes
        .iter()
        .find(|statement| statement.sql.starts_with("INSERT INTO loan_exit_review"))
        .unwrap();
    let expected_date = context.business_date().to_string();
    assert!(saved.to_string().contains(&format!("'{}'", expected_date)));
    let preview = match &saved.values.as_ref().unwrap().0[10] {
        sea_orm::Value::Json(Some(value)) => value,
        other => panic!("expected persisted exposure JSON, got {other:?}"),
    };
    assert_eq!(preview["asOf"], expected_date);
    assert_eq!(preview["settlementDate"], expected_date);
}

#[tokio::test]
async fn current_snapshot_date_passes_both_fnf_and_posting_date_gates() {
    for nonempty in [false, true] {
        let tenant = Uuid::new_v4();
        let context = metadata(tenant, true).await;
        let date = context.business_date();
        let subject = ExitSubject {
            separation_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(),
            last_working_date: date,
            status: "APPROVED".into(),
            fingerprint: "source".into(),
        };
        let claims = claims(tenant);
        let (db, quote) = company(tenant, &subject, date, nonempty).await;
        let actor = review::actor(&claims, tenant, &context).unwrap();
        let tx = db.begin().await.unwrap();
        // Company deliberately stops at financial source reads, after the date guard.
        let posted = kabipay_loans::post_fnf_recoveries(
            &tx,
            &PostRecoveriesInput {
                actor: &actor,
                quote: &quote,
            },
        )
        .await;
        assert!(matches!(posted, Err(LoanModuleError::Database(_))));
        drop(tx);
        let error = finalize::finalize(&db, tenant, &claims, &context, &subject, 1, "reviewed")
            .await
            .unwrap_err();
        assert!(
            matches!(error, KabiPayError::Internal(_)),
            "current date was rejected by readiness: {error:?}"
        );
    }
}
