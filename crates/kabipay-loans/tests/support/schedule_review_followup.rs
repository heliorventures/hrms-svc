use super::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn current_month_overdue_projection_keeps_anchor_and_remaining_recovery() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(1000),
        },
    )
    .await
    .unwrap();
    let first = f.today.with_day(1).unwrap();
    sql(&f.db, "INSERT INTO loan_schedule_version(id,tenant_id,loan_id,terms_version_id,version,effective_from,monthly_amount,recovery_mode,reason,actor_id,first_due_date) SELECT $1,tenant_id,loan_id,terms_version_id,2,$4,monthly_amount,recovery_mode,'Existing first-day agreement',actor_id,$4 FROM loan_schedule_version WHERE tenant_id=$2 AND loan_id=$3 AND version=1", vec![Uuid::new_v4().into(), f.tenant.into(), loan.into(), first.into()]).await;
    let tx = f.db.begin().await.unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut input = f.recovery(Uuid::new_v4(), 10000);
    input.source = RecoverySource::Payroll;
    input.available_net = Decimal::from(200);
    let quote = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    assert_eq!(quote.total(), Decimal::from(200));
    post_payroll_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
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
    assert_eq!(
        items[0]["total"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(300)
    );
    assert_eq!(items[0]["dueDate"], first.to_string());
    assert_eq!(items[0]["valueDate"], f.today.to_string());
    assert_eq!(
        items[1]["dueDate"],
        first
            .checked_add_months(Months::new(1))
            .unwrap()
            .to_string()
    );
    tx.rollback().await.unwrap();
    // A later reviewed agreement effective today supersedes the old overdue row.
    let next = first.checked_add_months(Months::new(1)).unwrap();
    f.apply(
        f.manager,
        "ALL",
        f.account(loan).await.version,
        "defer-by-agreement",
        LoanCommand::SetDeduction {
            loan_id: loan,
            effective_from: f.today,
            first_due_date: next,
            amount: Decimal::from(500),
            recovery: RecoveryMode::Payroll,
            reason: "Reviewed deferral".into(),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    let revised = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        f.today,
    )
    .await
    .unwrap();
    assert_eq!(revised["schedule"]["items"][0]["dueDate"], next.to_string());
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn unfunded_future_approval_only_accrues_from_permitted_funding_date() {
    let f = Fixture::new(100000).await;
    let request = f.request(f.user, "request").await;
    let funding = f.today.checked_add_months(Months::new(1)).unwrap();
    let first = funding.checked_add_months(Months::new(1)).unwrap();
    let terms = f.terms(InterestMethod::ReducingBalance {
        annual_rate: Decimal::from(12),
        convention: InterestConvention::Act365Fixed,
    });
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
                reason: "Funding agreed for next month".into(),
                terms: Some(terms.clone()),
                effective_from: Some(funding),
                first_due_date: Some(first),
                agreement_reference: Some("Agreed future funding".into()),
            },
        )
        .await
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let preview = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        approved.loan_id.unwrap(),
        first,
    )
    .await
    .unwrap();
    let expected = accrue_calendar_interest(
        &AccrualInput {
            principal: terms.approved_principal,
            method: terms.interest,
            start: funding,
            end_exclusive: first,
            carry: Decimal::ZERO,
            currency: terms.currency,
            rounding: terms.rounding,
        },
        &[],
    )
    .unwrap();
    assert_eq!(
        preview["schedule"]["items"][0]["interest"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        expected.posted_interest
    );
    assert_eq!(preview["fundingAssumptionDate"], funding.to_string());
    assert_eq!(preview["basis"], "APPROVED_UNFUNDED_BALANCE_PROJECTION");
    tx.rollback().await.unwrap();
}
