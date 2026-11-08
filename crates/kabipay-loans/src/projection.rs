//! Read-only schedules and exact policy references. Approval projections are never ledger balances.
use crate::{
    access, repository as store, LoanActorScope, LoanModuleError, LoanPermission, LoanPolicyView,
    LoanResult,
};
use chrono::NaiveDate;
use kabipay_loans_domain::{
    accrue_interest, project_schedule, AccrualInput, InterestMethod, LoanPolicy, LoanTerms,
    ScheduleInput,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub async fn loan_policy(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    id: Uuid,
) -> LoanResult<LoanPolicyView> {
    if !matches!(
        actor.permission,
        LoanPermission::Read | LoanPermission::Policy | LoanPermission::Approve
    ) {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    let row = store::policy(tx, actor.tenant_id, id).await?;
    Ok(LoanPolicyView {
        id: row.id,
        key: row.policy_key,
        version: row.version,
        status: row.status,
        currency: row.currency,
        minor_units: row.minor_units,
        effective_from: row.effective_from,
        effective_to: row.effective_to,
        rules: row.rules,
    })
}
pub async fn preview_approval_schedule(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    request_id: Uuid,
    terms: &LoanTerms,
    from: NaiveDate,
    first: NaiveDate,
) -> LoanResult<serde_json::Value> {
    if actor.permission != LoanPermission::Approve {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    let request = store::one(
        tx,
        "SELECT employee_id,preferences FROM loan_request WHERE tenant_id=$1 AND id=$2",
        vec![actor.tenant_id.into(), request_id.into()],
    )
    .await?
    .ok_or(LoanModuleError::NotFound)?;
    access::employee(tx, actor, request.try_get("", "employee_id")?).await?;
    let preferences: serde_json::Value = request.try_get("", "preferences")?;
    let policy_id: Uuid = serde_json::from_value(
        preferences
            .get("policyId")
            .cloned()
            .ok_or(LoanModuleError::InvalidCommand)?,
    )?;
    let policy = store::policy(tx, actor.tenant_id, policy_id).await?;
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    rules.validate_terms(terms)?;
    if terms.currency.code != policy.currency
        || i32::try_from(terms.currency.minor_units).ok() != Some(policy.minor_units)
    {
        return Err(LoanModuleError::InvalidCommand);
    }
    if first < from {
        return Err(LoanModuleError::InvalidCommand);
    }
    let interest = if matches!(
        terms.interest,
        InterestMethod::OneTimeFixed { .. } | InterestMethod::OneTimePercentage { .. }
    ) {
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
    let schedule = project_schedule(&ScheduleInput {
        terms: terms.clone(),
        principal: terms.approved_principal,
        due_interest: interest,
        carry: Decimal::ZERO,
        accrual_start: from,
        first_due_date: first,
        overrides: vec![],
    })?;
    Ok(
        serde_json::json!({"basis":"FULL_APPROVED_FUNDING_PROJECTION","fundingAssumptionDate":from,"currency":terms.currency,"schedule":schedule_value(schedule, from)?}),
    )
}
pub async fn loan_schedule_preview(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    first: NaiveDate,
) -> LoanResult<serde_json::Value> {
    if actor.permission != LoanPermission::Read {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    project_account(tx, actor, loan, Some(first), None, None).await
}

/// The current contractual schedule does not accept a replacement repayment date.
pub async fn current_loan_schedule_preview(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
) -> LoanResult<serde_json::Value> {
    if actor.permission != LoanPermission::Read {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    project_account(tx, actor, loan, None, None, None).await
}

pub(crate) use crate::account_projection::{calculate_account, DatedArrangement};

pub(crate) async fn project_account(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    first: Option<NaiveDate>,
    proposed: Option<DatedArrangement>,
    override_: Option<kabipay_loans_domain::PeriodOverride>,
) -> LoanResult<serde_json::Value> {
    let today = crate::lifecycle::today(tx, actor.tenant_id).await?;
    if first.is_some_and(|date| date < today) {
        return Err(LoanModuleError::InvalidCommand);
    }
    // The legacy caller date cannot replace a contractual anchor or hide a due row.
    let projected = calculate_account(tx, actor, loan, proposed, override_).await?;
    Ok(
        serde_json::json!({"basis":projected.basis,"accountVersion":projected.account_version,
        "fundingAssumptionDate":projected.funding_assumption_date,
        "currency":projected.terms.currency,"schedule":schedule_value(projected.schedule, projected.funding_assumption_date.unwrap_or(today))?}),
    )
}

fn schedule_value(
    schedule: kabipay_loans_domain::LoanSchedule,
    projection_start: NaiveDate,
) -> LoanResult<serde_json::Value> {
    let (projected_interest, projected_total) = schedule.items.iter().try_fold(
        (Decimal::ZERO, Decimal::ZERO),
        |(interest, total), item| -> LoanResult<_> {
            Ok((
                interest
                    .checked_add(item.interest)
                    .ok_or(LoanModuleError::InvalidCommand)?,
                total
                    .checked_add(item.total)
                    .ok_or(LoanModuleError::InvalidCommand)?,
            ))
        },
    )?;
    let last_date = schedule.items.last().map(|item| item.due_date);
    let fully_repaid =
        schedule.residual_principal.is_zero() && schedule.residual_interest.is_zero();
    let completion_status = if !fully_repaid {
        "RESIDUAL_REMAINING"
    } else if schedule.items.is_empty() {
        "NO_REMAINING_BALANCE"
    } else {
        "PROJECTED_PAYOFF"
    };
    let completion_date = fully_repaid
        .then(|| last_date.map(|date| date.max(projection_start)))
        .flatten();
    let items = schedule.items.into_iter().map(|item|serde_json::json!({"dueDate":item.due_date,"valueDate":item.due_date.max(projection_start),
        "principal":item.principal.to_string(),"interest":item.interest.to_string(),"total":item.total.to_string(),
        "principalAfter":item.principal_after.to_string(),"interestAfter":item.interest_after.to_string()})).collect::<Vec<_>>();
    Ok(
        serde_json::json!({"items":items,"residualPrincipal":schedule.residual_principal.to_string(),
        "residualInterest":schedule.residual_interest.to_string(),"carry":schedule.carry.to_string(),
        "completionStatus":completion_status,"expectedCompletionDate":completion_date,"lastScheduledDate":last_date,
        "projectedInterest":projected_interest.to_string(),"projectedTotalRepayment":projected_total.to_string()}),
    )
}
