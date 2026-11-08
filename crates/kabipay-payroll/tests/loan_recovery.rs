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

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn exit_payroll_context_separates_prepared_drafts_and_employee_issued_references() {
    let f = fixture::Fixture::new(100000).await;
    let draft = cycle(&f).await;
    fixture::sql(&f.db,"INSERT INTO payroll_period_input(id,tenant_id,employee_id,year,month) VALUES($1,$2,$3,$4,$5)",
        vec![Uuid::new_v4().into(), f.tenant.into(), f.employee.into(), draft.year.into(), draft.month.into()]).await;
    let issued = Uuid::new_v4();
    let slip = Uuid::new_v4();
    fixture::sql(
        &f.db,
        "INSERT INTO payroll_cycle(id,tenant_id,year,month,status) VALUES($1,$2,$3,$4,'PROCESSED')",
        vec![
            issued.into(),
            f.tenant.into(),
            draft.year.into(),
            draft.month.into(),
        ],
    )
    .await;
    fixture::sql(&f.db,"INSERT INTO payslip(id,tenant_id,employee_id,payroll_cycle_id,net_salary,status) VALUES($1,$2,$3,$4,9000,'GENERATED')",
        vec![slip.into(), f.tenant.into(), f.employee.into(), issued.into()]).await;
    let tx = f.db.begin().await.unwrap();
    let context = kabipay_payroll::fnf::payroll_context::read(&tx, f.tenant, f.employee)
        .await
        .unwrap();
    assert_eq!(context["draftCycles"][0]["id"], draft.id.to_string());
    assert_eq!(context["recentIssuedPayslips"][0]["id"], slip.to_string());
    assert_eq!(
        context["recentIssuedPayslips"][0]["amount"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(9000)
    );
    for (tenant, employee) in [(f.tenant, f.other), (Uuid::new_v4(), f.employee)] {
        let hidden = kabipay_payroll::fnf::payroll_context::read(&tx, tenant, employee)
            .await
            .unwrap();
        assert!(hidden["draftCycles"].as_array().unwrap().is_empty());
        assert!(hidden["recentIssuedPayslips"]
            .as_array()
            .unwrap()
            .is_empty());
    }
    tx.rollback().await.unwrap();
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

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn financial_reconciliation_holds_orphan_recovery_and_clears_only_after_source_proof() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let tx = f.db.begin().await.unwrap();
    let mut value = prepared(&f);
    loan_recovery::prepare(&tx, &claims, &cycle, 1, f.employee, &mut value)
        .await
        .unwrap();
    let snapshot = loan_recovery::post(&tx, &claims, value.loan_recovery.as_ref().unwrap())
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let report = kabipay_payroll::services::loan_reconciliation::reconcile_account(
        &tx,
        f.tenant,
        loan,
        Some(f.manager),
    )
    .await
    .unwrap();
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == "ISSUED_SOURCE_MISMATCH"));
    tx.commit().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let hidden_actor = f.actor(f.user, "SELF", LoanPermission::Manage);
    assert!(latest_loan_audit(&tx, &hidden_actor, loan).await.is_err());
    let hr_actor = f.actor(f.manager, "ALL", LoanPermission::Manage);
    assert_eq!(
        latest_loan_audit(&tx, &hr_actor, loan)
            .await
            .unwrap()
            .unwrap()
            .id,
        report.id
    );
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(tx
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE loan_reconciliation_status SET blocked=FALSE WHERE tenant_id=$1 AND id=$2",
            [f.tenant.into(), loan.into()]
        ))
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let mut next = prepared(&f);
    assert!(
        loan_recovery::prepare(&tx, &claims, &cycle, 2, f.employee, &mut next)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
    let slip = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO payslip(id,tenant_id,employee_id,payroll_cycle_id,net_salary,status) VALUES($1,$2,$3,$4,9500,'GENERATED')",vec![slip.into(),f.tenant.into(),f.employee.into(),cycle.id.into()]).await;
    for change_terms in [false, true] {
        let mut invalid = snapshot.clone();
        if change_terms {
            invalid.lines[0].terms_version_id = Uuid::new_v4();
        } else {
            invalid.quote_fingerprint = "b".repeat(64);
        }
        let tx = f.db.begin().await.unwrap();
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO payslip_loan_snapshot(payslip_id,tenant_id,employee_id,cycle_id,source_revision,value_date,currency,recovery_total,snapshot) VALUES($1,$2,$3,$4,1,$5,'INR',500,$6)",[slip.into(),f.tenant.into(),f.employee.into(),cycle.id.into(),f.today.into(),serde_json::to_value(invalid).unwrap().into()])).await.unwrap();
        tx.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE payroll_cycle SET status='PROCESSED' WHERE tenant_id=$1 AND id=$2",
            [f.tenant.into(), cycle.id.into()],
        ))
        .await
        .unwrap();
        let invalid = kabipay_payroll::services::loan_reconciliation::reconcile_account(
            &tx, f.tenant, loan, None,
        )
        .await
        .unwrap();
        assert!(
            invalid
                .issues
                .iter()
                .any(|issue| issue.code == "ISSUED_SOURCE_MISMATCH"),
            "A matching amount must not validate different reviewed terms or calculation evidence"
        );
        tx.rollback().await.unwrap();
    }
    fixture::sql(&f.db,"INSERT INTO payslip_loan_snapshot(payslip_id,tenant_id,employee_id,cycle_id,source_revision,value_date,currency,recovery_total,snapshot) VALUES($1,$2,$3,$4,1,$5,'INR',500,$6)",vec![slip.into(),f.tenant.into(),f.employee.into(),cycle.id.into(),f.today.into(),serde_json::to_value(&snapshot).unwrap().into()]).await;
    fixture::sql(
        &f.db,
        "UPDATE payroll_cycle SET status='PROCESSED' WHERE tenant_id=$1 AND id=$2",
        vec![f.tenant.into(), cycle.id.into()],
    )
    .await;
    let tx = f.db.begin().await.unwrap();
    let healthy = kabipay_payroll::services::loan_reconciliation::reconcile_account(
        &tx, f.tenant, loan, None,
    )
    .await
    .unwrap();
    assert!(healthy.issues.is_empty(), "{:?}", healthy.issues);
    assert_eq!(healthy.verified_source_count, 1);
    assert!(
        acknowledge_financial_events(&tx, f.tenant, 100)
            .await
            .unwrap()
            > 0
    );
    assert_eq!(
        acknowledge_financial_events(&tx, f.tenant, 100)
            .await
            .unwrap(),
        0
    );
    tx.commit().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(tx
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "UPDATE loan_reconciliation_run SET report='{}' WHERE id=$1",
            [report.id.into()]
        ))
        .await
        .is_err());
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(
        kabipay_payroll::services::loan_reconciliation::reconcile_account(
            &tx,
            Uuid::new_v4(),
            loan,
            None
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn financial_reconciliation_detects_issued_allocation_without_a_loan_posting() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let cycle = cycle(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let tx = f.db.begin().await.unwrap();
    let mut value = prepared(&f);
    loan_recovery::prepare(&tx, &claims, &cycle, 1, f.employee, &mut value)
        .await
        .unwrap();
    let snapshot = loan_recovery::post(&tx, &claims, value.loan_recovery.as_ref().unwrap())
        .await
        .unwrap();
    tx.rollback().await.unwrap();
    // Simulate missing Loan-side evidence without deleting an immutable production row.
    let slip = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO payslip(id,tenant_id,employee_id,payroll_cycle_id,net_salary,status) VALUES($1,$2,$3,$4,9500,'GENERATED')",vec![slip.into(),f.tenant.into(),f.employee.into(),cycle.id.into()]).await;
    fixture::sql(&f.db,"INSERT INTO payslip_loan_snapshot(payslip_id,tenant_id,employee_id,cycle_id,source_revision,value_date,currency,recovery_total,snapshot) VALUES($1,$2,$3,$4,1,$5,'INR',500,$6)",vec![slip.into(),f.tenant.into(),f.employee.into(),cycle.id.into(),f.today.into(),serde_json::to_value(snapshot).unwrap().into()]).await;
    fixture::sql(
        &f.db,
        "UPDATE payroll_cycle SET status='PROCESSED' WHERE tenant_id=$1 AND id=$2",
        vec![f.tenant.into(), cycle.id.into()],
    )
    .await;
    let tx = f.db.begin().await.unwrap();
    let report = kabipay_payroll::services::loan_reconciliation::reconcile_account(
        &tx, f.tenant, loan, None,
    )
    .await
    .unwrap();
    assert_eq!(report.verified_source_count, 0);
    assert!(report
        .issues
        .iter()
        .any(|issue| issue.code == "ISSUED_ALLOCATION_MISMATCH"));
    tx.rollback().await.unwrap();
    // A failed enclosing transaction must retain private events and leave no audit/status writes.
    let tx = f.db.begin().await.unwrap();
    assert!(latest_loan_audit(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Manage),
        loan
    )
    .await
    .unwrap()
    .is_none());
    assert!(due_financial_audits(&tx, f.tenant, 10)
        .await
        .unwrap()
        .contains(&loan));
    assert_eq!(
        acknowledge_financial_events(&tx, f.tenant, 100)
            .await
            .unwrap(),
        0
    );
    tx.rollback().await.unwrap();
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

async fn fnf_input(
    f: &mut fixture::Fixture,
) -> (
    Uuid,
    kabipay_common::finance_contract::ExitSubject,
    kabipay_payroll::fnf::model::FinancialInput,
) {
    use kabipay_loans_domain::LoanPolicy;
    use kabipay_payroll::fnf::{
        calculation::{ReviewedPayoutSource, SourceKind},
        model::FinancialInput,
    };
    let rules =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT rules FROM loan_policy_version WHERE tenant_id=$1 AND id=$2",
            [f.tenant.into(), f.policy.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<serde_json::Value>("", "rules")
        .unwrap();
    let mut rules: LoanPolicy = serde_json::from_value(rules).unwrap();
    rules.exposure.per_loan = 20000.into();
    rules.exposure.per_employee = 20000.into();
    f.policy = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fnf-policy",
            LoanCommand::PublishPolicy {
                key: "fnf-fixture".into(),
                currency: f.currency(),
                effective_from: f.today,
                effective_to: None,
                rules,
            },
        )
        .await
        .unwrap()
        .record_id;
    let request = f
        .apply(
            f.user,
            "SELF",
            1,
            "fnf-request",
            LoanCommand::SubmitRequest {
                request_id: None,
                employee_id: None,
                policy_id: f.policy,
                amount: 20000.into(),
                purpose: "FNF integration fixture".into(),
                notes: None,
                preferences: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    let mut approval = f.approval(request.record_id, InterestMethod::InterestFree);
    if let LoanCommand::DecideRequest {
        terms: Some(terms), ..
    } = &mut approval
    {
        terms.approved_principal = 20000.into();
        terms.monthly_amount = 1000.into();
    }
    let loan = f
        .apply(f.manager, "ALL", request.version, "fnf-approve", approval)
        .await
        .unwrap()
        .loan_id
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        1,
        "fnf-fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(20000),
        },
    )
    .await
    .unwrap();
    let separation = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO separation(id,tenant_id,employee_id,status,last_working_date) VALUES($1,$2,$3,'APPROVED',$4)",vec![separation.into(),f.tenant.into(),f.employee.into(),f.today.into()]).await;
    fixture::sql(
        &f.db,
        "INSERT INTO \"user\"(id,tenant_id,is_active,is_deleted) VALUES($1,$2,TRUE,FALSE)",
        vec![f.manager.into(), f.tenant.into()],
    )
    .await;
    let source = |kind, amount| ReviewedPayoutSource {
        kind,
        source_id: Uuid::new_v4(),
        amount,
        already_paid: Decimal::ZERO,
        other_recovery: Decimal::ZERO,
        eligible_for_loan: true,
        approval_reference: "fixture approved net benefit and eligible set-off".into(),
    };
    let subject = kabipay_common::finance_contract::ExitSubject {
        separation_id: separation,
        employee_id: f.employee,
        last_working_date: f.today,
        status: "APPROVED".into(),
        fingerprint: "c".repeat(64),
    };
    let input = FinancialInput {
        separation_id: separation,
        value_date: f.today,
        currency: f.currency(),
        exit_review_id: None,
        financial_approval_reference: "fixture HR benefit and tax review".into(),
        sources: vec![
            source(SourceKind::LeaveEncashment, 12000.into()),
            source(SourceKind::Gratuity, Decimal::ZERO),
            source(SourceKind::Bonus, Decimal::ZERO),
        ],
        legacy_review: None,
        loan_decision: ExitFinancialDecision {
            reason: "fixture recover eligible payout and continue residual debt".into(),
            loans: vec![ExitLoanDecision {
                loan_id: loan,
                final_payroll_cycle_id: None,
                proposed_payroll_recovery: Decimal::ZERO,
                proposed_fnf_recovery: 12000.into(),
                continuing: Some(ContinuingRepayment {
                    first_payment_date: f.today,
                    monthly_amount: 1000.into(),
                    follow_up_user_id: f.manager,
                    agreement_reference: "fixture approved residual agreement".into(),
                }),
            }],
        },
    };
    (loan, subject, input)
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn fnf_recovers_twelve_thousand_with_zero_cash_and_eight_thousand_debt_remaining() {
    use kabipay_payroll::fnf::{draft, finalize};
    let mut f = fixture::Fixture::new(100000).await;
    let (loan, subject, input) = fnf_input(&mut f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let review = draft::save(&f.db, f.tenant, &claims, &subject, &input, None, None)
        .await
        .unwrap();
    assert_eq!(review.snapshot.loan_recovered, 12000.into());
    assert_eq!(review.snapshot.employee_payout, Decimal::ZERO);
    assert_eq!(review.snapshot.residual_loan_receivable, 8000.into());
    assert_eq!(f.account(loan).await.principal, "20000.0000");
    let issued = finalize::finalize(
        &f.db,
        f.tenant,
        &claims,
        &subject,
        review.revision,
        &review.fingerprint,
        f.today,
    )
    .await
    .unwrap();
    assert!(issued.finalized);
    assert_eq!(f.account(loan).await.principal, "8000.0000");
    assert_eq!(
        finalize::finalize(
            &f.db,
            f.tenant,
            &claims,
            &subject,
            review.revision,
            &review.fingerprint,
            f.today
        )
        .await
        .unwrap()
        .snapshot
        .loan_recovered,
        12000.into()
    );
    let count:i64=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT COUNT(*) AS amount FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND source_kind='FNF' AND kind='REPAYMENT'",[f.tenant.into(),loan.into()])).await.unwrap().unwrap().try_get("","amount").unwrap();
    assert_eq!(count, 1);
    let tx = f.db.begin().await.unwrap();
    let report = kabipay_payroll::services::loan_reconciliation::reconcile_account(
        &tx, f.tenant, loan, None,
    )
    .await
    .unwrap();
    assert!(report.issues.is_empty(), "{:?}", report.issues);
    assert_eq!(report.verified_source_count, 1);
    tx.commit().await.unwrap();
    // Finalized FNF moves this balance to external servicing, even for an old payroll period.
    let tx = f.db.begin().await.unwrap();
    let payroll_actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut payroll_input = f.recovery(Uuid::new_v4(), 10000);
    payroll_input.source = RecoverySource::Payroll;
    assert_eq!(
        prepare_recovery_quote(&tx, &payroll_actor, &payroll_input)
            .await
            .unwrap()
            .total(),
        Decimal::ZERO
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn fnf_rejects_stale_repayment_and_rolls_back_a_failed_evidence_seal() {
    use kabipay_payroll::fnf::{draft, finalize};
    let mut f = fixture::Fixture::new(100000).await;
    let (loan, subject, input) = fnf_input(&mut f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let review = draft::save(&f.db, f.tenant, &claims, &subject, &input, None, None)
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        2,
        "fnf-concurrent-receipt",
        LoanCommand::RecordReceipt {
            loan_id: loan,
            payment: f.payment(1000),
        },
    )
    .await
    .unwrap();
    assert!(finalize::finalize(
        &f.db,
        f.tenant,
        &claims,
        &subject,
        review.revision,
        &review.fingerprint,
        f.today
    )
    .await
    .is_err());
    assert_eq!(f.account(loan).await.principal, "19000.0000");
    let review = draft::save(
        &f.db,
        f.tenant,
        &claims,
        &subject,
        &input,
        Some(review.revision),
        Some(&review.fingerprint),
    )
    .await
    .unwrap();
    fixture::sql(&f.db,"CREATE FUNCTION loan_test.fail_fnf_evidence_seal() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.finalized_at IS NOT NULL THEN RAISE EXCEPTION 'fixture seal failure'; END IF; RETURN NEW; END $$",vec![]).await;
    fixture::sql(&f.db,"CREATE TRIGGER fixture_fail_fnf_seal BEFORE UPDATE ON fnf_financial_review FOR EACH ROW EXECUTE FUNCTION loan_test.fail_fnf_evidence_seal()",vec![]).await;
    assert!(finalize::finalize(
        &f.db,
        f.tenant,
        &claims,
        &subject,
        review.revision,
        &review.fingerprint,
        f.today
    )
    .await
    .is_err());
    assert_eq!(f.account(loan).await.principal, "19000.0000");
    assert!(
        !draft::load(&f.db, f.tenant, review.fnf_id, false)
            .await
            .unwrap()
            .unwrap()
            .finalized
    );
    fixture::sql(
        &f.db,
        "DROP TRIGGER fixture_fail_fnf_seal ON fnf_financial_review",
        vec![],
    )
    .await;
    fixture::sql(
        &f.db,
        "DROP FUNCTION loan_test.fail_fnf_evidence_seal()",
        vec![],
    )
    .await;
    let issued = finalize::finalize(
        &f.db,
        f.tenant,
        &claims,
        &subject,
        review.revision,
        &review.fingerprint,
        f.today,
    )
    .await
    .unwrap();
    assert_eq!(issued.snapshot.residual_loan_receivable, 7000.into());
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn hr_no_loan_review_works_without_loans_but_never_hides_existing_exposure() {
    let f = fixture::Fixture::new(100000).await;
    fixture::sql(
        &f.db,
        "UPDATE kabipay_ops.module SET is_active=FALSE WHERE code='LOANS'",
        vec![],
    )
    .await;
    let separation = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO separation(id,tenant_id,employee_id,status,last_working_date) VALUES($1,$2,$3,'PENDING',$4)",vec![separation.into(),f.tenant.into(),f.employee.into(),f.today.into()]).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let actor =
        LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::FnfRecovery)
            .unwrap();
    let tx = f.db.begin().await.unwrap();
    let exposure = preview_exit_exposure(&tx, &actor, f.employee, f.today)
        .await
        .unwrap();
    assert!(exposure.loans.is_empty());
    let review = save_exit_review(
        &tx,
        &actor,
        &SaveExitReview {
            separation_id: separation,
            employee_id: f.employee,
            separation_fingerprint: "a".repeat(64),
            last_working_date: f.today,
            settlement_date: f.today,
            exposure_fingerprint: exposure.fingerprint,
            decision: ExitFinancialDecision {
                reason: "Verified no loan exposure".into(),
                loans: vec![],
            },
        },
    )
    .await
    .unwrap();
    assert!(review.exposure.loans.is_empty());
    tx.commit().await.unwrap();
    fixture::sql(
        &f.db,
        "UPDATE kabipay_ops.module SET is_active=TRUE WHERE code='LOANS'",
        vec![],
    )
    .await;
    let _loan = funded(&f).await;
    fixture::sql(
        &f.db,
        "UPDATE kabipay_ops.module SET is_active=FALSE WHERE code='LOANS'",
        vec![],
    )
    .await;
    let tx = f.db.begin().await.unwrap();
    assert!(preview_exit_exposure(&tx, &actor, f.employee, f.today)
        .await
        .is_err());
    tx.rollback().await.unwrap();
    fixture::sql(
        &f.db,
        "UPDATE kabipay_ops.module SET is_active=TRUE WHERE code='LOANS'",
        vec![],
    )
    .await;
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn exit_authorization_is_idempotent_and_releases_only_for_terminal_hr_state() {
    use kabipay_common::finance_contract::{FinanceAuthorizationState as F, HrOperationState as H};
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let separation = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO separation(id,tenant_id,employee_id,status,last_working_date) VALUES($1,$2,$3,'PENDING',$4)",vec![separation.into(),f.tenant.into(),f.employee.into(),f.today.into()]).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::ExitReview);
    let reviewer =
        LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::ExitReview)
            .unwrap();
    let tx = f.db.begin().await.unwrap();
    let exposure = preview_exit_exposure(&tx, &reviewer, f.employee, f.today)
        .await
        .unwrap();
    let review = save_exit_review(
        &tx,
        &reviewer,
        &SaveExitReview {
            separation_id: separation,
            employee_id: f.employee,
            separation_fingerprint: "a".repeat(64),
            last_working_date: f.today,
            settlement_date: f.today,
            exposure_fingerprint: exposure.fingerprint,
            decision: ExitFinancialDecision {
                reason: "HR reviewed external continuation".into(),
                loans: vec![ExitLoanDecision {
                    loan_id: loan,
                    final_payroll_cycle_id: None,
                    proposed_payroll_recovery: Decimal::ZERO,
                    proposed_fnf_recovery: Decimal::ZERO,
                    continuing: Some(ContinuingRepayment {
                        first_payment_date: f.today,
                        monthly_amount: Decimal::from(500),
                        follow_up_user_id: f.manager,
                        agreement_reference: "signed repayment agreement".into(),
                    }),
                }],
            },
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let claims = f.claims(f.manager, "ALL", LoanPermission::FnfRecovery);
    let actor =
        LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::FnfRecovery)
            .unwrap();
    let operation = RegisteredHrApproval {
        operation_id: Uuid::new_v4(),
        separation_id: separation,
        employee_id: f.employee,
        review_id: review.id,
        actor_id: f.manager,
        separation_fingerprint: "a".repeat(64),
        payload_hash: "b".repeat(64),
    };
    let tx = f.db.begin().await.unwrap();
    let authorization = authorize_exit_approval(&tx, &actor, &operation)
        .await
        .unwrap();
    assert_eq!(authorization.state, F::Authorized);
    assert_eq!(
        authorize_exit_approval(&tx, &actor, &operation)
            .await
            .unwrap()
            .id,
        authorization.id
    );
    assert_eq!(
        reconcile_exit_approval(&tx, f.tenant, &operation, None)
            .await
            .unwrap()
            .state,
        F::ReconciliationPending
    );
    assert_eq!(
        reconcile_exit_approval(&tx, f.tenant, &operation, Some(H::Registered))
            .await
            .unwrap()
            .state,
        F::ReconciliationPending
    );
    tx.commit().await.unwrap();
    assert!(f
        .apply(
            f.manager,
            "ALL",
            2,
            "held-receipt",
            LoanCommand::RecordReceipt {
                loan_id: loan,
                payment: f.payment(100)
            }
        )
        .await
        .is_err());
    let tx = f.db.begin().await.unwrap();
    assert_eq!(
        reconcile_exit_approval(&tx, f.tenant, &operation, Some(H::Committed))
            .await
            .unwrap()
            .state,
        F::Confirmed
    );
    tx.commit().await.unwrap();
    let row = f.db.query_one(sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS count FROM loan_exit_arrangement WHERE tenant_id=$1 AND loan_id=$2 AND origin_kind='HR_APPROVAL'",
        [f.tenant.into(),loan.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "count").unwrap(), 1);
    let tx = f.db.begin().await.unwrap();
    let payroll_actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut payroll_input = f.recovery(Uuid::new_v4(), 10000);
    payroll_input.source = RecoverySource::Payroll;
    payroll_input.value_date = f.today.succ_opt().unwrap();
    assert_eq!(
        prepare_recovery_quote(&tx, &payroll_actor, &payroll_input)
            .await
            .unwrap()
            .total(),
        Decimal::ZERO
    );
    assert!(
        reconcile_exit_approval(&tx, f.tenant, &operation, Some(H::Committed))
            .await
            .is_ok()
    );
    tx.commit().await.unwrap();
    f.apply(
        f.manager,
        "ALL",
        2,
        "held-receipt",
        LoanCommand::RecordReceipt {
            loan_id: loan,
            payment: f.payment(100),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(
        reconcile_exit_approval(&tx, f.tenant, &operation, Some(H::Cancelled))
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn exit_exposure_is_read_only_and_requires_explicit_residual_decision() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let claims = f.claims(f.manager, "ALL", LoanPermission::ExitReview);
    let actor = LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::ExitReview)
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let exposure = preview_exit_exposure(&tx, &actor, f.employee, f.today)
        .await
        .unwrap();
    assert_eq!(exposure.loans.len(), 1);
    assert_eq!(exposure.loans[0].principal, Decimal::from(10000));
    assert_eq!(exposure.loans[0].forecast_payoff, Decimal::from(10000));
    assert_eq!(exposure.loans[0].unfunded_commitment, Decimal::ZERO);
    assert_eq!(exposure.loans[0].unapplied_credit, Decimal::ZERO);
    assert!(validate_exit_decision(
        &exposure,
        &ExitFinancialDecision {
            reason: "reviewed".into(),
            loans: vec![]
        }
    )
    .is_err());
    let incomplete = ExitFinancialDecision {
        reason: "HR reviewed the settlement".into(),
        loans: vec![ExitLoanDecision {
            loan_id: loan,
            final_payroll_cycle_id: None,
            proposed_payroll_recovery: Decimal::ZERO,
            proposed_fnf_recovery: Decimal::ZERO,
            continuing: None,
        }],
    };
    assert!(validate_exit_decision(&exposure, &incomplete).is_err());
    tx.commit().await.unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn exit_guard_does_not_expire_into_unreviewed_financial_writes() {
    let f = fixture::Fixture::new(100000).await;
    let loan = funded(&f).await;
    let separation = Uuid::new_v4();
    let review = Uuid::new_v4();
    let operation = Uuid::new_v4();
    fixture::sql(&f.db, "INSERT INTO separation(id,tenant_id,employee_id,status,last_working_date) VALUES($1,$2,$3,'PENDING',$4)", vec![separation.into(),f.tenant.into(),f.employee.into(),f.today.into()]).await;
    fixture::sql(&f.db, "INSERT INTO loan_exit_review(id,tenant_id,separation_id,employee_id,actor_id,separation_fingerprint,last_working_date,settlement_date,employee_revision,quote_fingerprint,preview,decision,decision_hash) VALUES($1,$2,$3,$4,$5,repeat('a',64),$6,$6,3,repeat('b',64),'{}','{}',repeat('c',64))", vec![review.into(),f.tenant.into(),separation.into(),f.employee.into(),f.manager.into(),f.today.into()]).await;
    fixture::sql(&f.db, "INSERT INTO employee_exit_operation(id,tenant_id,separation_id,employee_id,actor_id,review_id,separation_fingerprint,payload_hash,state) VALUES($1,$2,$3,$4,$5,$6,repeat('a',64),repeat('c',64),'REGISTERED')", vec![operation.into(),f.tenant.into(),separation.into(),f.employee.into(),f.manager.into(),review.into()]).await;
    fixture::sql(&f.db, "INSERT INTO loan_exit_approval_guard(id,tenant_id,employee_id,separation_id,review_id,operation_id,actor_id,payload_hash,state,expires_at) VALUES($1,$2,$3,$4,$5,$6,$7,repeat('c',64),'AUTHORIZED',NOW()-INTERVAL '1 minute')", vec![Uuid::new_v4().into(),f.tenant.into(),f.employee.into(),separation.into(),review.into(),operation.into(),f.manager.into()]).await;
    for state in ["AUTHORIZED", "RECONCILIATION_PENDING"] {
        fixture::sql(
            &f.db,
            "UPDATE loan_exit_approval_guard SET state=$2 WHERE tenant_id=$1",
            vec![f.tenant.into(), state.into()],
        )
        .await;
        assert!(f
            .apply(
                f.manager,
                "ALL",
                2,
                "guarded-receipt",
                LoanCommand::RecordReceipt {
                    loan_id: loan,
                    payment: f.payment(100)
                }
            )
            .await
            .is_err());
        assert_eq!(
            f.account(loan).await.principal.parse::<Decimal>().unwrap(),
            Decimal::from(10000)
        );
    }
    fixture::sql(
        &f.db,
        "UPDATE loan_exit_approval_guard SET state='CANCELLED' WHERE tenant_id=$1",
        vec![f.tenant.into()],
    )
    .await;
    f.apply(
        f.manager,
        "ALL",
        2,
        "guarded-receipt",
        LoanCommand::RecordReceipt {
            loan_id: loan,
            payment: f.payment(100),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(9900)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn issued_fnf_review_figures_and_payout_sources_cannot_change() {
    let f = fixture::Fixture::new(100000).await;
    let separation = Uuid::new_v4();
    let settlement = Uuid::new_v4();
    let review = Uuid::new_v4();
    fixture::sql(&f.db,"INSERT INTO separation(id,tenant_id,employee_id,status,last_working_date) VALUES($1,$2,$3,'APPROVED',$4)",vec![separation.into(),f.tenant.into(),f.employee.into(),f.today.into()]).await;
    fixture::sql(&f.db,"INSERT INTO fnf_settlement(id,tenant_id,separation_id,status,leave_encashment,recovery_amount,net_payable) VALUES($1,$2,$3,'DRAFT',12000,12000,0)",vec![settlement.into(),f.tenant.into(),separation.into()]).await;
    fixture::sql(&f.db,"INSERT INTO fnf_financial_review(id,tenant_id,fnf_id,employee_id,revision,value_date,currency,minor_units,input_hash,snapshot,actor_id) VALUES($1,$2,$3,$4,1,$5,'INR',2,repeat('a',64),'{}',$6)",vec![review.into(),f.tenant.into(),settlement.into(),f.employee.into(),f.today.into(),f.manager.into()]).await;
    fixture::sql(&f.db,"INSERT INTO fnf_payout_source(id,tenant_id,fnf_id,source_kind,source_id,amount,already_paid,other_recovery,eligible_for_loan,approval_reference) VALUES($1,$2,$3,'LEAVE_ENCASHMENT',$4,12000,0,0,TRUE,'approved company set-off review')",vec![Uuid::new_v4().into(),f.tenant.into(),settlement.into(),Uuid::new_v4().into()]).await;
    fixture::sql(
        &f.db,
        "UPDATE fnf_financial_review SET finalized_at=NOW() WHERE id=$1",
        vec![review.into()],
    )
    .await;
    fixture::sql(&f.db,"UPDATE fnf_settlement SET status='PROCESSED',processed_at=NOW(),processed_by=$2 WHERE id=$1",vec![settlement.into(),f.manager.into()]).await;
    for query in [
        "UPDATE fnf_settlement SET net_payable=12000 WHERE id=$1",
        "DELETE FROM fnf_settlement WHERE id=$1",
        "UPDATE fnf_financial_review SET snapshot='{}' WHERE fnf_id=$1",
        "DELETE FROM fnf_payout_source WHERE fnf_id=$1",
        "INSERT INTO fnf_payout_source(id,tenant_id,fnf_id,source_kind,source_id,amount,already_paid,other_recovery,eligible_for_loan,approval_reference) SELECT gen_random_uuid(),tenant_id,id,'BONUS',gen_random_uuid(),100,0,0,TRUE,'late mutation' FROM fnf_settlement WHERE id=$1",
    ] {
        assert!(f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres,query,vec![settlement.into()])).await.is_err(),"{query}");
    }
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
