use super::*;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn late_receipt_replays_interest_and_preserves_issued_recovery_and_retry() {
    let f = Fixture::new(100000).await;
    let loan = f
        .loan(InterestMethod::ReducingBalance {
            annual_rate: 12.into(),
            convention: InterestConvention::Act365Fixed,
        })
        .await;
    let start = f.today - chrono::Duration::days(30);
    let terms_id = Uuid::new_v4();
    sql(&f.db,"INSERT INTO loan_terms_version(id,tenant_id,loan_id,policy_version_id,version,effective_from,terms,approved_by,agreement_evidence) SELECT $3,tenant_id,loan_id,policy_version_id,2,$4,terms,approved_by,agreement_evidence FROM loan_terms_version WHERE tenant_id=$1 AND loan_id=$2 AND version=1",vec![f.tenant.into(),loan.into(),terms_id.into(),start.into()]).await;
    sql(
        &f.db,
        "UPDATE loan_account SET current_terms_id=$3 WHERE tenant_id=$1 AND id=$2",
        vec![f.tenant.into(), loan.into(), terms_id.into()],
    )
    .await;
    let mut payment = f.payment(10000);
    payment.value_date = start;
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment,
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::FnfRecovery);
    let quote = prepare_recovery_quote(&tx, &actor, &f.recovery(Uuid::new_v4(), 1000))
        .await
        .unwrap();
    let issued = post_fnf_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let frozen = issued.posting_ids.last().copied().unwrap();
    let before = f.account(loan).await;
    let mut payment = f.payment(1000);
    payment.value_date = start + chrono::Duration::days(10);
    let mut claims = f.claims(f.manager, "ALL", LoanPermission::Repay);
    let tx = f.db.begin().await.unwrap();
    assert!(preview_historical_payment(
        &tx,
        &LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::Repay).unwrap(),
        loan,
        HistoricalPaymentKind::Receipt,
        &payment
    )
    .await
    .is_err());
    tx.rollback().await.unwrap();
    claims.permissions.push("loan:correct".into());
    claims
        .permission_scopes
        .insert("loan:correct".into(), "ALL".into());
    let actor =
        LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::Repay).unwrap();
    let tx = f.db.begin().await.unwrap();
    let preview =
        preview_historical_payment(&tx, &actor, loan, HistoricalPaymentKind::Receipt, &payment)
            .await
            .unwrap();
    assert_eq!(preview.corrected_principal, Decimal::new(809227, 2));
    assert_eq!(preview.corrected_interest, Decimal::ZERO);
    assert!(preview.preserved_payroll_postings.contains(&frozen));
    tx.rollback().await.unwrap();
    assert_eq!(f.account(loan).await.version, before.version);
    let input = LoanCommand::RecordHistoricalPayment {
        loan_id: loan,
        kind: HistoricalPaymentKind::Receipt,
        payment: payment.clone(),
        reason: "Missed external receipt confirmed by HR".into(),
        reconciliation_reference: "reviewed-payment-1".into(),
        review_fingerprint: preview.review_fingerprint,
    };
    let meta = CommandMeta {
        idempotency_key: "late-receipt".into(),
        expected_version: before.version,
    };
    let tx = f.db.begin().await.unwrap();
    let mut altered = input.clone();
    if let LoanCommand::RecordHistoricalPayment {
        review_fingerprint, ..
    } = &mut altered
    {
        *review_fingerprint = "b".repeat(64);
    }
    assert!(matches!(
        execute_command(&tx, &actor, &meta, &altered).await,
        Err(LoanModuleError::VersionConflict)
    ));
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let result = execute_command(&tx, &actor, &meta, &input).await.unwrap();
    assert_eq!(
        execute_command(&tx, &actor, &meta, &input)
            .await
            .unwrap()
            .record_id,
        result.record_id
    );
    tx.commit().await.unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::new(809227, 2)
    );
    let row=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT r.value_date AS occurrence,p.value_date AS posting FROM loan_receipt r JOIN loan_posting p ON p.tenant_id=r.tenant_id AND p.source_id=r.id AND p.source_kind='EXTERNAL_RECEIPT' WHERE r.tenant_id=$1 AND r.id=$2",[f.tenant.into(),result.record_id.into()])).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<NaiveDate>("", "occurrence").unwrap(),
        payment.value_date
    );
    assert_eq!(row.try_get::<NaiveDate>("", "posting").unwrap(), f.today);
    let original=f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT principal_delta::text AS principal,interest_delta::text AS interest FROM loan_posting WHERE tenant_id=$1 AND id=$2",[f.tenant.into(),frozen.into()])).await.unwrap().unwrap();
    assert_eq!(
        original
            .try_get::<String>("", "interest")
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::new(-9863, 2)
    );
    // Reversing the new late receipt must replay its occurrence date, not its current posting date.
    let tx = f.db.begin().await.unwrap();
    let correction = f.actor(f.manager, "ALL", LoanPermission::Correct);
    let review = preview_loan_reversal(&tx, &correction, loan, result.posting_ids[0])
        .await
        .unwrap();
    assert_eq!(review.corrected_principal, Decimal::new(909863, 2));
    tx.rollback().await.unwrap();
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn late_funding_keeps_original_date_charges_and_pending_resignation_evidence() {
    let f = Fixture::new(100000).await;
    let loan = f
        .loan(InterestMethod::OneTimePercentage { rate: 10.into() })
        .await;
    let start = f.today - chrono::Duration::days(30);
    let terms_id = Uuid::new_v4();
    sql(&f.db,"INSERT INTO loan_terms_version(id,tenant_id,loan_id,policy_version_id,version,effective_from,terms,approved_by,agreement_evidence) SELECT $3,tenant_id,loan_id,policy_version_id,2,$4,terms,approved_by,agreement_evidence FROM loan_terms_version WHERE tenant_id=$1 AND loan_id=$2 AND version=1",vec![f.tenant.into(),loan.into(),terms_id.into(),start.into()]).await;
    sql(
        &f.db,
        "UPDATE loan_account SET current_terms_id=$3 WHERE tenant_id=$1 AND id=$2",
        vec![f.tenant.into(), loan.into(), terms_id.into()],
    )
    .await;
    let mut payment = f.payment(5000);
    payment.value_date = start;
    f.apply(
        f.manager,
        "ALL",
        1,
        "initial-funding",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment,
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    let recovery_actor = f.actor(f.manager, "ALL", LoanPermission::FnfRecovery);
    let quote = prepare_recovery_quote(&tx, &recovery_actor, &f.recovery(Uuid::new_v4(), 1000))
        .await
        .unwrap();
    post_fnf_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &recovery_actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let mut payment = f.payment(2000);
    payment.value_date = start + chrono::Duration::days(5);
    payment.resignation_exception_reason =
        Some("HR and management approved funding exception".into());
    let mut claims = f.claims(f.manager, "ALL", LoanPermission::Disburse);
    claims.permissions.push("loan:correct".into());
    claims
        .permission_scopes
        .insert("loan:correct".into(), "ALL".into());
    let actor =
        LoanActorScope::from_verified_claims(&claims, f.tenant, LoanPermission::Disburse).unwrap();
    let tx = f.db.begin().await.unwrap();
    let review = preview_historical_payment(
        &tx,
        &actor,
        loan,
        HistoricalPaymentKind::Disbursement,
        &payment,
    )
    .await
    .unwrap();
    assert_eq!(review.corrected_principal, Decimal::from(6700));
    assert_eq!(review.corrected_interest, Decimal::ZERO);
    let context = kabipay_common::finance_contract::FundingContext {
        employee_id: f.employee,
        value_date: payment.value_date,
        pending_separation_id: Some(Uuid::new_v4()),
    };
    let input = LoanCommand::RecordHistoricalPayment {
        loan_id: loan,
        kind: HistoricalPaymentKind::Disbursement,
        payment: payment.clone(),
        reason: "Late funding evidence confirmed".into(),
        reconciliation_reference: "HR funding review".into(),
        review_fingerprint: review.review_fingerprint,
    };
    let result = execute_with_funding_context(
        &tx,
        &actor,
        &CommandMeta {
            idempotency_key: "late-funding".into(),
            expected_version: review.account_version,
        },
        &input,
        Some(&context),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(6700)
    );
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT value_date,evidence FROM loan_disbursement WHERE tenant_id=$1 AND id=$2",
            [f.tenant.into(), result.record_id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        row.try_get::<NaiveDate>("", "value_date").unwrap(),
        payment.value_date
    );
    assert_eq!(
        row.try_get::<serde_json::Value>("", "evidence").unwrap()["fundingContext"]
            ["pendingSeparationId"],
        context.pending_separation_id.unwrap().to_string()
    );
}
