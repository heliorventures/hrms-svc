use super::*;

async fn quote_at(f: &Fixture, date: NaiveDate) -> LoanResult<RecoveryQuote> {
    let tx = f.db.begin().await.unwrap();
    let mut input = f.recovery(Uuid::new_v4(), 10000);
    input.source = RecoverySource::Payroll;
    input.value_date = date;
    input.period_start = date.with_day(1).unwrap();
    let result = prepare_recovery_quote(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery),
        &input,
    )
    .await;
    tx.rollback().await.unwrap();
    result
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn residual_recovery_continues_after_forecast_rows_within_approved_tenure() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
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
    // Approval projects 20 instalments; missed recoveries must still be due in month 21.
    let quote = quote_at(&f, f.today.checked_add_months(Months::new(21)).unwrap())
        .await
        .unwrap();
    assert_eq!(quote.total(), Decimal::from(500));
    let error = quote_at(&f, f.today.checked_add_months(Months::new(25)).unwrap())
        .await
        .unwrap_err();
    assert!(error.to_string().contains("HR review"), "{error}");
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn deduction_before_funding_keeps_contractual_first_due_date() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let first = f.today.checked_add_months(Months::new(2)).unwrap();
    let changed = f
        .apply(
            f.manager,
            "ALL",
            1,
            "arrange",
            LoanCommand::SetDeduction {
                loan_id: loan,
                effective_from: f.today,
                first_due_date: first,
                amount: Decimal::from(1000),
                recovery: RecoveryMode::Payroll,
                reason: "Agreed before funding".into(),
            },
        )
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        changed.version,
        "fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(10000),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        quote_at(&f, f.today.checked_add_months(Months::new(1)).unwrap())
            .await
            .unwrap()
            .total(),
        Decimal::ZERO
    );
    assert_eq!(
        quote_at(&f, first).await.unwrap().total(),
        Decimal::from(1000)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn preview_uses_future_arrangement_on_its_anchored_due_dates() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
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
    let first = f.today.checked_add_months(Months::new(1)).unwrap();
    let future = f
        .today
        .checked_add_months(Months::new(2))
        .unwrap()
        .with_day(1)
        .unwrap();
    let future_first = future
        .checked_add_months(Months::new(1))
        .unwrap()
        .pred_opt()
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        funded.version,
        "arrange",
        LoanCommand::SetDeduction {
            loan_id: loan,
            effective_from: future,
            first_due_date: future_first,
            amount: Decimal::from(1000),
            recovery: RecoveryMode::Payroll,
            reason: "Future agreed amount".into(),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    let preview = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        first,
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
        Decimal::from(500)
    );
    assert_eq!(items[1]["dueDate"], future_first.to_string());
    assert_eq!(
        items[1]["total"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(1000)
    );
    assert_eq!(
        items[2]["dueDate"],
        future_first
            .checked_add_months(Months::new(1))
            .unwrap()
            .to_string()
    );
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn preview_interest_matches_recovery_across_a_paused_calendar_month() {
    let f = Fixture::new(100000).await;
    let loan = f
        .loan(InterestMethod::ReducingBalance {
            annual_rate: Decimal::from(12),
            convention: InterestConvention::Act365Fixed,
        })
        .await;
    let funded = f
        .apply(
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
    let first = f.today.checked_add_months(Months::new(2)).unwrap();
    let arranged = f
        .apply(
            f.manager,
            "ALL",
            funded.version,
            "arrange",
            LoanCommand::SetDeduction {
                loan_id: loan,
                effective_from: f.today,
                first_due_date: first,
                amount: Decimal::from(500),
                recovery: RecoveryMode::Payroll,
                reason: "Agreed first recovery".into(),
            },
        )
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        arranged.version,
        "pause",
        LoanCommand::SetPeriodOverride {
            loan_id: loan,
            period_start: f
                .today
                .checked_add_months(Months::new(1))
                .unwrap()
                .with_day(1)
                .unwrap(),
            amount: None,
            pause_interest: true,
            reason: "Approved calendar interest holiday".into(),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    let preview = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        first,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let quote = quote_at(&f, first).await.unwrap();
    assert_eq!(
        preview["schedule"]["items"][0]["interest"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        quote.lines()[0].accrued_interest
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn missing_historical_anchor_requires_hr_review_and_explicit_arrangement_recovers() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
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
    // Represents a historical pre-funding revision with no forecast evidence.
    sql(&f.db, "INSERT INTO loan_schedule_version(id,tenant_id,loan_id,terms_version_id,version,effective_from,monthly_amount,recovery_mode,reason,actor_id) SELECT $1,tenant_id,loan_id,terms_version_id,2,effective_from,monthly_amount,recovery_mode,'Legacy empty forecast',actor_id FROM loan_schedule_version WHERE tenant_id=$2 AND loan_id=$3 AND version=1", vec![Uuid::new_v4().into(), f.tenant.into(), loan.into()]).await;
    let first = f.today.checked_add_months(Months::new(1)).unwrap();
    assert!(matches!(
        quote_at(&f, first).await,
        Err(LoanModuleError::RepaymentReviewRequired)
    ));
    f.apply(
        f.manager,
        "ALL",
        funded.version,
        "reviewed-anchor",
        LoanCommand::SetDeduction {
            loan_id: loan,
            effective_from: f.today,
            first_due_date: first,
            amount: Decimal::from(500),
            recovery: RecoveryMode::Payroll,
            reason: "HR confirmed contractual first due date".into(),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        quote_at(&f, first).await.unwrap().total(),
        Decimal::from(500)
    );
}
