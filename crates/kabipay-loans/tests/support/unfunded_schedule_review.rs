use super::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn unfunded_current_month_due_is_recoverable_on_assumed_funding_date() {
    let f = Fixture::new(100000).await;
    let request = f.request(f.user, "request").await;
    let mut terms = f.terms(InterestMethod::InterestFree);
    terms.monthly_amount = terms.approved_principal;
    terms.max_instalments = 1;
    let approved = f
        .apply(
            f.manager,
            "ALL",
            request.version,
            "approve",
            LoanCommand::DecideRequest {
                request_id: request.record_id,
                step_id: f.step,
                decision: RequestDecision::Approve,
                reason: "One agreed instalment".into(),
                terms: Some(terms),
                effective_from: Some(f.today),
                first_due_date: Some(f.today.checked_add_months(Months::new(1)).unwrap()),
                agreement_reference: Some("Single repayment agreement".into()),
            },
        )
        .await
        .unwrap();
    let loan = approved.loan_id.unwrap();
    let first = f.today.with_day(1).unwrap();
    // Existing immutable contract evidence whose first due date has arrived while unfunded.
    sql(&f.db, "INSERT INTO loan_schedule_version(id,tenant_id,loan_id,terms_version_id,version,effective_from,monthly_amount,recovery_mode,reason,actor_id,first_due_date) SELECT $1,tenant_id,loan_id,terms_version_id,2,$4,monthly_amount,recovery_mode,'Existing first-day single-instalment agreement',actor_id,$4 FROM loan_schedule_version WHERE tenant_id=$2 AND loan_id=$3 AND version=1", vec![Uuid::new_v4().into(), f.tenant.into(), loan.into(), first.into()]).await;
    let tx = f.db.begin().await.unwrap();
    let preview = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        f.today,
    )
    .await
    .unwrap();
    let items = preview["schedule"]["items"].as_array().unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0]["dueDate"], first.to_string());
    assert_eq!(items[0]["valueDate"], f.today.to_string());
    assert_eq!(preview["fundingAssumptionDate"], f.today.to_string());
    let projected = items[0]["total"]
        .as_str()
        .unwrap()
        .parse::<Decimal>()
        .unwrap();
    assert_eq!(projected, Decimal::from(10000));
    tx.rollback().await.unwrap();
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
    let tx = f.db.begin().await.unwrap();
    let mut input = f.recovery(Uuid::new_v4(), 30000);
    input.source = RecoverySource::Payroll;
    let quote = prepare_recovery_quote(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery),
        &input,
    )
    .await
    .unwrap();
    assert_eq!(quote.total(), projected);
    tx.rollback().await.unwrap();
}
