use super::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn zero_balance_projection_reports_no_remaining_balance() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f.apply(f.manager, "ALL", 1, "settled-fund", LoanCommand::RecordDisbursement { loan_id: loan, payment: f.payment(1000) }).await.unwrap();
    f.apply(f.manager, "ALL", funded.version, "settled-receipt", LoanCommand::RecordReceipt { loan_id: loan, payment: f.payment(1000) }).await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let preview = current_loan_schedule_preview(&tx, &f.actor(f.user, "SELF", LoanPermission::Read), loan).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(preview["schedule"]["items"].as_array().unwrap().is_empty());
    assert_eq!(preview["schedule"]["completionStatus"], "NO_REMAINING_BALANCE");
}

fn normalized_schedule(mut value: serde_json::Value) -> serde_json::Value {
    fn normalize(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::String(text) => {
                if let Ok(amount) = text.parse::<Decimal>() {
                    *text = amount.normalize().to_string();
                }
            }
            serde_json::Value::Array(items) => items.iter_mut().for_each(normalize),
            serde_json::Value::Object(fields) => fields.values_mut().for_each(normalize),
            _ => (),
        }
    }
    normalize(&mut value);
    value
}

async fn evidence_counts(f: &Fixture, loan: Uuid) -> Vec<i64> {
    let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT (SELECT count(*) FROM loan_schedule_version WHERE tenant_id=$1 AND loan_id=$2) AS schedules, (SELECT count(*) FROM loan_schedule_item WHERE tenant_id=$1 AND loan_id=$2) AS items, (SELECT count(*) FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2) AS postings, (SELECT count(*) FROM loan_command_receipt WHERE tenant_id=$1) AS receipts, (SELECT count(*) FROM loan_period_override WHERE tenant_id=$1 AND loan_id=$2) AS overrides",
        vec![f.tenant.into(), loan.into()])).await.unwrap().unwrap();
    ["schedules", "items", "postings", "receipts", "overrides"]
        .iter()
        .map(|name| row.try_get("", name).unwrap())
        .collect()
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn proposed_arrangements_are_read_only_and_match_the_saved_projection() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "preview-fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    let first = f.today.checked_add_months(Months::new(1)).unwrap();
    let input = LoanCommand::SetDeduction {
        loan_id: loan,
        effective_from: f.today,
        first_due_date: first,
        amount: Decimal::from(1000),
        recovery: RecoveryMode::Payroll,
        reason: "Reviewed revised monthly amount".into(),
    };
    let before = evidence_counts(&f, loan).await;
    let tx = f.db.begin().await.unwrap();
    let preview = preview_arrangement_change(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Manage),
        funded.version,
        &input,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        evidence_counts(&f, loan).await,
        before,
        "even a committed preview must not persist records"
    );
    assert_eq!(f.account(loan).await.version, funded.version);
    assert_eq!(preview["schedule"]["items"].as_array().unwrap().len(), 10);
    assert_eq!(
        preview["schedule"]["expectedCompletionDate"],
        first
            .checked_add_months(Months::new(9))
            .unwrap()
            .to_string()
    );
    assert_eq!(
        preview["schedule"]["projectedInterest"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::ZERO
    );
    assert_eq!(
        preview["schedule"]["projectedTotalRepayment"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(10000)
    );
    let saved = f
        .apply(f.manager, "ALL", funded.version, "preview-save", input)
        .await
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let actual = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        first,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        normalized_schedule(preview["schedule"].clone()),
        normalized_schedule(actual["schedule"].clone())
    );

    let skip = LoanCommand::SetPeriodOverride {
        loan_id: loan,
        period_start: first.with_day(1).unwrap(),
        amount: None,
        pause_interest: false,
        reason: "Reviewed skip".into(),
    };
    let before = evidence_counts(&f, loan).await;
    let tx = f.db.begin().await.unwrap();
    let preview = preview_arrangement_change(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Manage),
        saved.version,
        &skip,
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(evidence_counts(&f, loan).await, before);
    assert_eq!(
        preview["schedule"]["items"][0]["total"]
            .as_str()
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::ZERO
    );
    f.apply(f.manager, "ALL", saved.version, "preview-skip", skip)
        .await
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let actual = loan_schedule_preview(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        first,
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    assert_eq!(
        normalized_schedule(preview["schedule"].clone()),
        normalized_schedule(actual["schedule"].clone())
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn arrangement_previews_require_current_version_authority_and_valid_amounts() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "preview-fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    let period = f
        .today
        .checked_add_months(Months::new(1))
        .unwrap()
        .with_day(1)
        .unwrap();
    let invalid = LoanCommand::SetPeriodOverride {
        loan_id: loan,
        period_start: period,
        amount: Some(Decimal::ZERO),
        pause_interest: false,
        reason: "Invalid zero is not a skip".into(),
    };
    let tx = f.db.begin().await.unwrap();
    assert!(matches!(
        preview_arrangement_change(
            &tx,
            &f.actor(f.manager, "ALL", LoanPermission::Manage),
            funded.version,
            &invalid
        )
        .await,
        Err(LoanModuleError::InvalidCommand)
    ));
    tx.rollback().await.unwrap();
    assert!(matches!(
        f.apply(f.manager, "ALL", funded.version, "invalid-zero", invalid)
            .await,
        Err(LoanModuleError::InvalidCommand)
    ));
    let valid = LoanCommand::SetPeriodOverride {
        loan_id: loan,
        period_start: period,
        amount: None,
        pause_interest: false,
        reason: "Requested skip".into(),
    };
    let tx = f.db.begin().await.unwrap();
    assert!(matches!(
        preview_arrangement_change(
            &tx,
            &f.actor(f.manager, "ALL", LoanPermission::Manage),
            1,
            &valid
        )
        .await,
        Err(LoanModuleError::VersionConflict)
    ));
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(matches!(
        preview_arrangement_change(
            &tx,
            &f.actor(f.user, "SELF", LoanPermission::Read),
            funded.version,
            &valid
        )
        .await,
        Err(LoanModuleError::Forbidden)
    ));
    tx.rollback().await.unwrap();
}
