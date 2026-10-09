use kabipay_loans::*;
use kabipay_loans_domain::InterestMethod;
use kabipay_payroll::services::{
    automatic_payroll::PreparedEmployeePayroll, loan_recovery,
    payroll_draft::FinalizeAcknowledgement,
};
use kabipay_payroll::services::{loan_recovery::apply_recovery, payroll_rules::CalculatedPeriod};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;
#[path = "../../kabipay-loans/tests/support/fixture.rs"]
#[allow(dead_code)]
mod fixture;

fn prepared(f: &fixture::Fixture) -> PreparedEmployeePayroll {
    serde_json::from_value(serde_json::json!({
        "input": {
            "year":chrono::Datelike::year(&f.today),"month":chrono::Datelike::month(&f.today),
            "gross_rule":"SOURCE_OVERRIDE", "fixed_gross":"10000", "earned_gross_override":"10000",
            "lwp_days":"0", "lwp_divisor":"30", "lwp_basis":"GROSS",
            "lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION", "variable_allowance_ot":"0",
            "incentive":"0", "advance_already_paid":"0", "additional_deductions":[],
            "statutory_overrides":{"PF":"0","ESI":"0","PT":"0","TDS":"0"},
            "expected_earned_components":{"BASIC":"10000"}, "expected_employer_contributions":{},
            "expected_statement":{}, "historical_lwp_included":false,"ready":true
        },
        "calculation": {
            "gross":"10000","incentive":"0","total_deductions":"0","net_earned":"10000",
            "advance_already_paid":"0","remaining_payable":"10000","lwp_amount":"0",
            "lwp_days":"0","lwp_divisor":"30","lwp_basis_amount":"10000",
            "gross_rule":"SOURCE_OVERRIDE","components":{"BASIC":"10000"},"statutory":{},
            "employer":{},"additional_deductions":[]
        },
        "tax_projection":null,"contribution_evidence":null,"requires_tax_acknowledgement":false
    }))
    .unwrap()
}

async fn funded(f: &fixture::Fixture) -> Uuid {
    let request = f.request(f.user, "request").await;
    let mut approval = f.approval(request.record_id, InterestMethod::InterestFree);
    if let LoanCommand::DecideRequest { first_due_date, .. } = &mut approval {
        *first_due_date = Some(f.today);
    }
    let loan = f
        .apply(f.manager, "ALL", request.version, "approve", approval)
        .await
        .unwrap()
        .loan_id
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(10000),
        },
    )
    .await
    .unwrap();
    loan
}

async fn cycle(
    f: &fixture::Fixture,
) -> kabipay_db_entities::tenant::d0012_payroll::payroll_cycle::Model {
    let id = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO payroll_cycle(id,tenant_id,name,year,month,status,payment_date) VALUES($1,$2,'reviewed loan payroll',$3,$4,'DRAFT',$5)",vec![id.into(),f.tenant.into(),chrono::Datelike::year(&f.today).into(),(chrono::Datelike::month(&f.today) as i32).into(),f.today.into()]).await;
    kabipay_payroll::services::payroll_draft::cycle(&f.db, f.tenant, id)
        .await
        .unwrap()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn payroll_quote_is_read_only_and_payment_changes_require_new_review() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let tx = f.db.begin().await.unwrap();
    let mut value = prepared(&f);
    loan_recovery::prepare(&tx, &claims, &cycle, 1, f.employee, &mut value)
        .await
        .unwrap();
    assert_eq!(value.calculation.loan_recovery, Decimal::from(500));
    let reviewed = value.loan_recovery.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
    f.apply(
        f.manager,
        "ALL",
        2,
        "receipt",
        LoanCommand::RecordReceipt {
            loan_id: loan,
            payment: f.payment(9900),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(loan_recovery::post(&tx, &claims, &reviewed).await.is_err());
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let mut current = prepared(&f);
    loan_recovery::prepare(&tx, &claims, &cycle, 2, f.employee, &mut current)
        .await
        .unwrap();
    assert_eq!(current.calculation.loan_recovery, Decimal::from(100));
    let snapshot = loan_recovery::post(&tx, &claims, current.loan_recovery.as_ref().unwrap())
        .await
        .unwrap();
    assert_eq!(
        snapshot.lines[0]
            .principal_after
            .parse::<Decimal>()
            .unwrap(),
        Decimal::ZERO
    );
    tx.rollback().await.unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(100)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn no_loan_review_is_invalidated_by_new_loan_and_missing_payment_date_blocks() {
    let f = fixture::Fixture::new(100000).await;
    let mut cycle = cycle(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let tx = f.db.begin().await.unwrap();
    let before = loan_recovery::fingerprint(&tx, f.tenant).await.unwrap();
    let mut value = prepared(&f);
    cycle.payment_date = None;
    assert!(
        loan_recovery::prepare(&tx, &claims, &cycle, 1, f.employee, &mut value)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    funded(&f).await;
    let tx = f.db.begin().await.unwrap();
    assert_ne!(
        before,
        loan_recovery::fingerprint(&tx, f.tenant).await.unwrap()
    );
    tx.rollback().await.unwrap();
}

async fn monthly_input(f: &fixture::Fixture) {
    fixture::sql(
        &f.db,
        "UPDATE employee SET payroll_excluded=(id<>$2),employee_code=id::text WHERE tenant_id=$1",
        vec![f.tenant.into(), f.employee.into()],
    )
    .await;
    fixture::sql(&f.db,"INSERT INTO salary_component(id,tenant_id,code,name,type,is_active,is_taxable,is_fixed) VALUES($1,$2,'BASIC','Basic','EARNING',TRUE,TRUE,TRUE)",vec![Uuid::new_v4().into(),f.tenant.into()]).await;
    let value = prepared(f).input;
    fixture::sql(&f.db,"INSERT INTO payroll_period_input(id,tenant_id,employee_id,year,month,input,revision,ready) VALUES($1,$2,$3,$4,$5,$6,1,TRUE)",vec![Uuid::new_v4().into(),f.tenant.into(),f.employee.into(),value.year.into(),value.month.into(),serde_json::to_value(value).unwrap().into()]).await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn future_payment_date_allows_preview_but_blocks_finalization_without_financial_writes() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    monthly_input(&f).await;
    let payment_date = f.today.checked_add_days(chrono::Days::new(1)).unwrap();
    fixture::sql(
        &f.db,
        "UPDATE payroll_cycle SET payment_date=$2 WHERE id=$1",
        vec![cycle.id.into(), payment_date.into()],
    )
    .await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let draft = kabipay_payroll::services::payroll_draft::calculate_payroll_cycle(
        &f.db, f.tenant, &claims, cycle.id, None,
    )
    .await
    .unwrap();
    let entry = draft
        .employees
        .iter()
        .find(|entry| entry.employee_id == f.employee)
        .unwrap();
    assert_eq!(entry.outcome, "READY");
    assert_eq!(
        entry.prepared.as_ref().unwrap().calculation.loan_recovery,
        Decimal::from(500)
    );
    assert!(!draft.can_finalize, "future payroll must remain a preview");
    let value = serde_json::to_value(&draft).unwrap();
    let reason = value["finalization_block_reason"].as_str().unwrap();
    assert!(reason.contains(&payment_date.to_string()));
    let error = kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        draft.revision,
        &draft.fingerprint,
        FinalizeAcknowledgement::default(),
    )
    .await
    .unwrap_err();
    assert!(
        error.to_string().contains(&payment_date.to_string()),
        "{error}"
    );
    let reloaded = kabipay_payroll::services::payroll_draft::find(&f.db, f.tenant, cycle.id)
        .await
        .unwrap()
        .unwrap();
    assert!(!reloaded.can_finalize);
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
    let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT (SELECT COUNT(*) FROM payslip WHERE tenant_id=$1) AS slips, (SELECT COUNT(*) FROM payslip_loan_snapshot WHERE tenant_id=$1) AS snapshots",
        vec![f.tenant.into()],
    )).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "slips").unwrap(), 0);
    assert_eq!(row.try_get::<i64>("", "snapshots").unwrap(), 0);
    assert_eq!(
        kabipay_payroll::services::payroll_draft::cycle(&f.db, f.tenant, cycle.id)
            .await
            .unwrap()
            .status,
        "DRAFT"
    );

    // Today's date remains eligible; correcting the date still requires a new review.
    kabipay_payroll::services::payroll_draft::set_payment_date(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        f.today,
        Some(draft.revision),
    )
    .await
    .unwrap();
    let current = kabipay_payroll::services::payroll_draft::calculate_payroll_cycle(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        Some(draft.revision),
    )
    .await
    .unwrap();
    assert!(current.can_finalize);
    kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        current.revision,
        &current.fingerprint,
        FinalizeAcknowledgement::default(),
    )
    .await
    .unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(9500)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn finalization_commits_loan_payslip_and_cycle_atomically_and_snapshots_are_immutable() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    monthly_input(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let draft = kabipay_payroll::services::payroll_draft::calculate_payroll_cycle(
        &f.db, f.tenant, &claims, cycle.id, None,
    )
    .await
    .unwrap();
    assert!(draft.can_finalize, "{draft:?}");
    let outcome = kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        draft.revision,
        &draft.fingerprint,
        FinalizeAcknowledgement::default(),
    )
    .await
    .unwrap();
    assert_eq!(outcome.payslips, 1);
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(9500)
    );
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT p.id,p.net_salary::text AS net,s.snapshot FROM payslip p JOIN payslip_loan_snapshot s ON s.tenant_id=p.tenant_id AND s.payslip_id=p.id WHERE p.tenant_id=$1 AND p.employee_id=$2",vec![f.tenant.into(),f.employee.into()])).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<String>("", "net")
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(9500)
    );
    let snapshot = row.try_get::<serde_json::Value>("", "snapshot").unwrap();
    assert_eq!(
        snapshot["lines"][0]["principalAfter"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(9500)
    );
    assert!(!snapshot["postingIds"].as_array().unwrap().is_empty());
    let slip = row.try_get::<Uuid>("", "id").unwrap();
    // Test JSON constraints independently of the immutable-record trigger.
    // PostgreSQL CHECK accepts UNKNOWN unless the predicate explicitly requires TRUE.
    for field in ["sourceId", "quoteFingerprint", "total"] {
        let tx = f.db.begin().await.unwrap();
        tx.execute(Statement::from_string(
            DbBackend::Postgres,
            "DROP TRIGGER guard_payslip_loan_snapshot ON payslip_loan_snapshot",
        ))
        .await
        .unwrap();
        let result = tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE payslip_loan_snapshot SET snapshot=jsonb_set(snapshot,ARRAY[$2::text],'null'::jsonb) WHERE payslip_id=$1",
            vec![slip.into(), field.into()],
        )).await;
        tx.rollback().await.unwrap();
        assert!(
            result.is_err(),
            "null {field} must not satisfy financial evidence constraints"
        );
    }
    for statement in [
        "UPDATE payslip_loan_snapshot SET snapshot='{}'::jsonb WHERE payslip_id=$1",
        "DELETE FROM payslip_loan_snapshot WHERE payslip_id=$1",
        "UPDATE payslip SET net_salary=0 WHERE id=$1",
        "UPDATE payslip_statement SET statement='{}'::jsonb WHERE payslip_id=$1",
    ] {
        assert!(f
            .db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                statement,
                vec![slip.into()]
            ))
            .await
            .is_err());
    }
    assert!(
        kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
            &f.db,
            f.tenant,
            &claims,
            cycle.id,
            draft.revision,
            &draft.fingerprint,
            FinalizeAcknowledgement::default()
        )
        .await
        .is_err()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn posting_failure_rolls_back_the_payslip_and_preserves_draft_and_loan() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    monthly_input(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let draft = kabipay_payroll::services::payroll_draft::calculate_payroll_cycle(
        &f.db, f.tenant, &claims, cycle.id, None,
    )
    .await
    .unwrap();
    assert!(draft.can_finalize, "{draft:?}");
    // Failure is injected after financial posting but before commit, at evidence persistence.
    fixture::sql(&f.db,"CREATE OR REPLACE FUNCTION reject_fixture_snapshot() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'injected evidence failure'; END $$",vec![]).await;
    fixture::sql(&f.db,"CREATE TRIGGER reject_fixture_snapshot BEFORE INSERT ON payslip_loan_snapshot FOR EACH ROW EXECUTE FUNCTION reject_fixture_snapshot()",vec![]).await;
    let result = kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        draft.revision,
        &draft.fingerprint,
        FinalizeAcknowledgement::default(),
    )
    .await;
    fixture::sql(
        &f.db,
        "DROP TRIGGER reject_fixture_snapshot ON payslip_loan_snapshot",
        vec![],
    )
    .await;
    assert!(result.is_err());
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
    assert_eq!(
        kabipay_payroll::services::payroll_draft::cycle(&f.db, f.tenant, cycle.id)
            .await
            .unwrap()
            .status,
        "DRAFT"
    );
    let count =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM payslip WHERE tenant_id=$1",
            vec![f.tenant.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(count.try_get::<i64>("", "n").unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn concurrent_receipt_wins_financial_lock_and_stale_payroll_posts_nothing() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    monthly_input(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let draft = kabipay_payroll::services::payroll_draft::calculate_payroll_cycle(
        &f.db, f.tenant, &claims, cycle.id, None,
    )
    .await
    .unwrap();
    assert!(draft.can_finalize, "{draft:?}");
    let tx = f.db.begin().await.unwrap();
    execute_command(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Repay),
        &CommandMeta {
            idempotency_key: "concurrent-receipt".into(),
            expected_version: 2,
        },
        &LoanCommand::RecordReceipt {
            loan_id: loan,
            payment: f.payment(9900),
        },
    )
    .await
    .unwrap();
    let connection = f.db.clone();
    let tenant = f.tenant;
    let mut finalize = tokio::spawn(async move {
        kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
            &connection,
            tenant,
            &claims,
            cycle.id,
            draft.revision,
            &draft.fingerprint,
            FinalizeAcknowledgement::default(),
        )
        .await
    });
    assert!(
        tokio::time::timeout(std::time::Duration::from_millis(50), &mut finalize)
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let error = finalize.await.unwrap().unwrap_err();
    assert!(error.to_string().contains("changed"), "{error}");
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(100)
    );
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM payslip WHERE tenant_id=$1",
            vec![f.tenant.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn loan_recovery_defers_when_salary_is_paid_or_only_a_small_cash_balance_remains() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    for cash in [0, 1200] {
        let tx = f.db.begin().await.unwrap();
        let mut value = prepared(&f);
        value.input.advance_already_paid = Some((10000 - cash).to_string());
        value.calculation =
            kabipay_payroll::services::payroll_rules::calculate_period(&value.input).unwrap();
        loan_recovery::prepare(&tx, &claims, &cycle, 1, f.employee, &mut value)
            .await
            .unwrap();
        let expected = if cash == 0 { 0 } else { 200 };
        assert_eq!(value.calculation.loan_recovery, Decimal::from(expected));
        assert_eq!(
            value.loan_recovery.as_ref().unwrap().quote.lines()[0].deferred,
            Decimal::from(500 - expected)
        );
        assert_eq!(
            value.calculation.remaining_payable,
            Decimal::from(cash - expected)
        );
        tx.rollback().await.unwrap();
    }
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn earlier_salary_cannot_be_spent_again_by_a_supplementary_recovery_quote() {
    let f = fixture::Fixture::new(100000).await;
    funded(&f).await;
    let prior = cycle(&f).await;
    fixture::sql(
        &f.db,
        "UPDATE payroll_cycle SET status='PROCESSED' WHERE id=$1",
        vec![prior.id.into()],
    )
    .await;
    // A historical salary record deliberately has no invented loan repayment evidence.
    fixture::sql(&f.db,"INSERT INTO payslip(id,tenant_id,employee_id,payroll_cycle_id,gross_salary,net_salary,total_deductions) VALUES($1,$2,$3,$4,10000,10000,0)",vec![Uuid::new_v4().into(),f.tenant.into(),f.employee.into(),prior.id.into()]).await;
    let current = cycle(&f).await;
    let mut value = prepared(&f);
    value.input.advance_already_paid = Some("9900".into());
    value.calculation =
        kabipay_payroll::services::payroll_rules::calculate_period(&value.input).unwrap();
    let tx = f.db.begin().await.unwrap();
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    loan_recovery::prepare(&tx, &claims, &current, 1, f.employee, &mut value)
        .await
        .unwrap();
    assert_eq!(value.calculation.loan_recovery, Decimal::from(100));
    assert_eq!(value.calculation.remaining_payable, Decimal::ZERO);
    let quote = &value.loan_recovery.as_ref().unwrap().quote;
    assert_eq!(quote.input().eligible_net, Decimal::from(10100));
    assert_eq!(quote.input().available_net, Decimal::from(100));
    assert_eq!(quote.lines()[0].deferred, Decimal::from(400));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn setting_draft_payment_date_requires_latest_review_and_invalidates_old_fingerprint() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    monthly_input(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let draft = kabipay_payroll::services::payroll_draft::calculate_payroll_cycle(
        &f.db, f.tenant, &claims, cycle.id, None,
    )
    .await
    .unwrap();
    let date = f.today + chrono::Duration::days(1);
    assert!(kabipay_payroll::services::payroll_draft::set_payment_date(
        &f.db, f.tenant, &claims, cycle.id, date, None
    )
    .await
    .is_err());
    kabipay_payroll::services::payroll_draft::set_payment_date(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        date,
        Some(draft.revision),
    )
    .await
    .unwrap();
    assert!(
        kabipay_payroll::services::payroll_finalize::finalize_payroll_cycle(
            &f.db,
            f.tenant,
            &claims,
            cycle.id,
            draft.revision,
            &draft.fingerprint,
            FinalizeAcknowledgement::default()
        )
        .await
        .is_err()
    );
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
    assert_eq!(
        kabipay_payroll::services::payroll_draft::cycle(&f.db, f.tenant, cycle.id)
            .await
            .unwrap()
            .payment_date,
        Some(date)
    );
    assert!(kabipay_payroll::services::payroll_draft::set_payment_date(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        f.today,
        Some(draft.revision)
    )
    .await
    .is_err());
    fixture::sql(
        &f.db,
        "UPDATE payroll_cycle SET status='PROCESSED' WHERE id=$1",
        vec![cycle.id.into()],
    )
    .await;
    assert!(kabipay_payroll::services::payroll_draft::set_payment_date(
        &f.db,
        f.tenant,
        &claims,
        cycle.id,
        f.today,
        Some(draft.revision)
    )
    .await
    .is_err());
}

fn calculation() -> CalculatedPeriod {
    serde_json::from_value(serde_json::json!({
        "gross":"10000", "incentive":"0", "total_deductions":"1000",
        "net_earned":"9000", "advance_already_paid":"8500", "remaining_payable":"500",
        "lwp_amount":"0", "lwp_days":"0", "lwp_divisor":"30", "lwp_basis_amount":"10000",
        "gross_rule":"SOURCE_OVERRIDE", "components":{"BASIC":"10000"},
        "statutory":{"PF":"1000"}, "employer":{"pf":"1200"}, "additional_deductions":[]
    }))
    .unwrap()
}

#[test]
fn ledger_recovery_changes_cash_deductions_without_changing_earnings_or_salary_paid() {
    let mut value = calculation();
    apply_recovery(&mut value, Decimal::from(400)).unwrap();
    assert_eq!(value.gross, Decimal::from(10000));
    assert_eq!(value.total_deductions, Decimal::from(1400));
    assert_eq!(value.net_earned, Decimal::from(8600));
    assert_eq!(value.advance_already_paid, Decimal::from(8500));
    assert_eq!(value.remaining_payable, Decimal::from(100));
    assert_eq!(value.loan_recovery, Decimal::from(400));
    assert_eq!(value.employer["pf"], "1200");
    assert!(value.additional_deductions.is_empty());
}

#[test]
fn recovery_cannot_consume_salary_already_paid_or_be_applied_twice() {
    let mut value = calculation();
    assert!(apply_recovery(&mut value, Decimal::from(501)).is_err());
    assert_eq!(value.remaining_payable, Decimal::from(500));
    apply_recovery(&mut value, Decimal::from(500)).unwrap();
    assert!(apply_recovery(&mut value, Decimal::ONE).is_err());
    assert_eq!(value.remaining_payable, Decimal::ZERO);
}

#[test]
fn manual_deductions_cannot_impersonate_ledger_backed_loan_recovery() {
    for code in ["LOAN", "LOAN_RECOVERY", "LOAN_INTEREST", "LOAN_PRINCIPAL"] {
        assert!(!kabipay_payroll::services::payroll_rules::valid_additional_code(code));
        assert!(
            kabipay_payroll::services::unpaid_leave_policy::ensure_manual_component(code).is_err()
        );
    }
}
