use crate::{
    access, lifecycle, posting, repository as store, LoanActorScope, LoanCommandResult,
    LoanModuleError, LoanResult,
};
use chrono::{Datelike, NaiveDate};
use kabipay_loans_domain::{
    accrue_interest, project_schedule, AccrualInput, InterestMethod, LoanPolicy, LoanTerms,
    PeriodOverride, RecoveryMode, ScheduleInput,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) async fn overrides(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
) -> LoanResult<Vec<PeriodOverride>> {
    store::all(tx,"SELECT period_start,amount::text AS amount,accrual_treatment FROM loan_period_override WHERE tenant_id=$1 AND loan_id=$2 ORDER BY period_start",vec![tenant.into(),loan.into()]).await?.into_iter().map(|row|{
        let amount:Option<String>=row.try_get("","amount")?;
        Ok(PeriodOverride{period:row.try_get("","period_start")?,amount:amount.map(|value|value.parse().map_err(|_|LoanModuleError::InvalidCommand)).transpose()?,pause_interest:row.try_get::<String>("","accrual_treatment")?=="PAUSE"})
    }).collect()
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn initial(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    terms_id: Uuid,
    terms: &LoanTerms,
    from: NaiveDate,
    first: NaiveDate,
    reason: &str,
) -> LoanResult<()> {
    let one_time = matches!(
        terms.interest,
        InterestMethod::OneTimeFixed { .. } | InterestMethod::OneTimePercentage { .. }
    );
    let due_interest = if one_time {
        accrue_interest(&AccrualInput {
            principal: terms.approved_principal,
            method: terms.interest.clone(),
            start: from,
            end_exclusive: from,
            carry: Decimal::ZERO,
            currency: terms.currency.clone(),
            rounding: terms.rounding,
        })?
        .posted_interest
    } else {
        Decimal::ZERO
    };
    persist(
        tx,
        actor,
        loan,
        terms_id,
        terms,
        from,
        first,
        terms.approved_principal,
        due_interest,
        Decimal::ZERO,
        reason,
    )
    .await
}
#[allow(clippy::too_many_arguments)]
async fn persist(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    terms_id: Uuid,
    terms: &LoanTerms,
    from: NaiveDate,
    first: NaiveDate,
    principal: Decimal,
    interest: Decimal,
    carry: Decimal,
    reason: &str,
) -> LoanResult<()> {
    let schedule = project_schedule(&ScheduleInput {
        terms: terms.clone(),
        principal,
        due_interest: interest,
        carry,
        accrual_start: from,
        first_due_date: first,
        overrides: overrides(tx, actor.tenant_id, loan).await?,
    })?;
    let row=store::one(tx,"SELECT COALESCE(MAX(version),0)+1 AS version FROM loan_schedule_version WHERE tenant_id=$1 AND loan_id=$2",vec![actor.tenant_id.into(),loan.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let version: i64 = row.try_get("", "version")?;
    let id = Uuid::new_v4();
    let recovery = match terms.recovery {
        RecoveryMode::Payroll => "PAYROLL",
        RecoveryMode::External => "EXTERNAL",
        RecoveryMode::Mixed => "MIXED",
    };
    store::execute(tx,"INSERT INTO loan_schedule_version(id,tenant_id,loan_id,terms_version_id,version,effective_from,monthly_amount,recovery_mode,reason,actor_id) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10)",vec![id.into(),actor.tenant_id.into(),loan.into(),terms_id.into(),version.into(),from.into(),terms.monthly_amount.into(),recovery.into(),reason.into(),actor.user_id.into()]).await?;
    for item in schedule.items {
        store::execute(tx,"INSERT INTO loan_schedule_item(id,tenant_id,loan_id,schedule_version_id,due_date,principal,interest,total) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",vec![Uuid::new_v4().into(),actor.tenant_id.into(),loan.into(),id.into(),item.due_date.into(),item.principal.into(),item.interest.into(),item.total.into()]).await?;
    }
    Ok(())
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn deduction(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    loan: Uuid,
    from: NaiveDate,
    first: NaiveDate,
    amount: Decimal,
    recovery: RecoveryMode,
    reason: &str,
) -> LoanResult<LoanCommandResult> {
    lifecycle::text(reason, 2000)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    lifecycle::checked_version(account.version, expected)?;
    access::employee(tx, actor, account.employee_id).await?;
    if account.state != "OPEN"
        || from < lifecycle::today(tx, actor.tenant_id).await?
        || first < from
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    let version = store::terms(tx, &account).await?;
    let mut terms: LoanTerms = serde_json::from_value(version.terms)?;
    terms.monthly_amount = amount;
    terms.recovery = recovery;
    let policy = store::policy(tx, actor.tenant_id, version.policy_version_id).await?;
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    rules.validate_terms(&terms)?;
    let (principal, interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let projected = crate::accrual::calculate(
        tx,
        actor.tenant_id,
        loan,
        &terms,
        principal,
        account.accrued_through,
        from,
        account.rounding_carry,
    )
    .await?;
    persist(
        tx,
        actor,
        loan,
        version.id,
        &terms,
        from,
        first,
        principal,
        interest
            .checked_add(projected.amount)
            .ok_or(LoanModuleError::InvalidCommand)?,
        projected.carry,
        reason,
    )
    .await?;
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    Ok(lifecycle::result(
        loan,
        Some(loan),
        Some(account.employee_id),
        version,
        "ARRANGEMENT_UPDATED",
    ))
}
#[allow(clippy::too_many_arguments)]
pub(crate) async fn period(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    loan: Uuid,
    start: NaiveDate,
    amount: Option<Decimal>,
    pause: bool,
    reason: &str,
) -> LoanResult<LoanCommandResult> {
    lifecycle::text(reason, 2000)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    lifecycle::checked_version(account.version, expected)?;
    access::employee(tx, actor, account.employee_id).await?;
    let today = lifecycle::today(tx, actor.tenant_id).await?;
    if start.day() != 1
        || start < today.with_day(1).ok_or(LoanModuleError::InvalidCommand)?
        || account.state != "OPEN"
    {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    let previous=store::one(tx,"SELECT accrual_treatment FROM loan_period_override WHERE tenant_id=$1 AND loan_id=$2 AND period_start=$3",vec![actor.tenant_id.into(),loan.into(),start.into()]).await?;
    let was_paused = previous
        .map(|row| row.try_get::<String>("", "accrual_treatment"))
        .transpose()?
        .is_some_and(|value| value == "PAUSE");
    if pause != was_paused && account.accrued_through.is_some_and(|date| date > start) {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    let terms_version = store::terms(tx, &account).await?;
    let terms: LoanTerms = serde_json::from_value(terms_version.terms)?;
    let policy = store::policy(tx, actor.tenant_id, terms_version.policy_version_id).await?;
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    rules.validate_terms(&terms)?;
    if amount.is_some() && !rules.recovery.allow_overrides
        || amount.is_none() && !rules.recovery.allow_skips
        || pause && !rules.recovery.allow_accrual_pause
    {
        return Err(LoanModuleError::Forbidden);
    }
    if let Some(amount) = amount {
        terms.currency.validate_amount(amount)?;
        if amount <= Decimal::ZERO {
            return Err(LoanModuleError::InvalidCommand);
        }
    }
    // A posted salary recovery freezes the period's recovery decision.
    if store::one(tx,"SELECT id FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND source_kind='PAYROLL' AND calculation_evidence->>'periodStart'=$3::date::text LIMIT 1",vec![actor.tenant_id.into(),loan.into(),start.into()]).await?.is_some(){return Err(LoanModuleError::VersionConflict)}
    store::execute(tx,"INSERT INTO loan_period_override(id,tenant_id,loan_id,period_start,action,amount,accrual_treatment,reason,actor_id,version) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,1) ON CONFLICT(tenant_id,loan_id,period_start) DO UPDATE SET action=EXCLUDED.action,amount=EXCLUDED.amount,accrual_treatment=EXCLUDED.accrual_treatment,reason=EXCLUDED.reason,actor_id=EXCLUDED.actor_id,version=loan_period_override.version+1",vec![Uuid::new_v4().into(),actor.tenant_id.into(),loan.into(),start.into(),if amount.is_some(){"AMOUNT"}else{"SKIP"}.into(),amount.into(),if pause{"PAUSE"}else{"CONTINUE"}.into(),reason.into(),actor.user_id.into()]).await?;
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    Ok(lifecycle::result(
        loan,
        Some(loan),
        Some(account.employee_id),
        version,
        "PERIOD_UPDATED",
    ))
}
