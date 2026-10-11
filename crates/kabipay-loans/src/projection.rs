//! Read-only schedules and exact policy references. Approval projections are never ledger balances.
use crate::{access, accrual, arrangement, repository as store, LoanActorScope, LoanModuleError, LoanPermission, LoanPolicyView, LoanResult};
use chrono::NaiveDate;
use kabipay_loans_domain::{accrue_interest, project_schedule, AccrualInput, InterestMethod, LoanPolicy, LoanTerms, ScheduleInput};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub async fn loan_policy(tx: &DatabaseTransaction, actor: &LoanActorScope, id: Uuid) -> LoanResult<LoanPolicyView> {
    if !matches!(actor.permission,LoanPermission::Read|LoanPermission::Policy|LoanPermission::Approve) { return Err(LoanModuleError::Forbidden); }
    access::entitled(tx,actor).await?;
    let row = store::policy(tx,actor.tenant_id,id).await?;
    Ok(LoanPolicyView { id:row.id,key:row.policy_key,version:row.version,status:row.status,currency:row.currency,
        minor_units:row.minor_units,effective_from:row.effective_from,effective_to:row.effective_to,rules:row.rules })
}
pub async fn preview_approval_schedule(
    tx: &DatabaseTransaction, actor: &LoanActorScope, request_id: Uuid,
    terms: &LoanTerms, from: NaiveDate, first: NaiveDate,
) -> LoanResult<serde_json::Value> {
    if actor.permission != LoanPermission::Approve { return Err(LoanModuleError::Forbidden); }
    access::entitled(tx,actor).await?;
    let request = store::one(tx,"SELECT employee_id,preferences FROM loan_request WHERE tenant_id=$1 AND id=$2",
        vec![actor.tenant_id.into(),request_id.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    access::employee(tx,actor,request.try_get("","employee_id")?).await?;
    let preferences: serde_json::Value = request.try_get("","preferences")?;
    let policy_id: Uuid = serde_json::from_value(preferences.get("policyId").cloned().ok_or(LoanModuleError::InvalidCommand)?)?;
    let policy = store::policy(tx,actor.tenant_id,policy_id).await?;
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    rules.validate_terms(terms)?;
    if terms.currency.code != policy.currency || i32::try_from(terms.currency.minor_units).ok() != Some(policy.minor_units) {
        return Err(LoanModuleError::InvalidCommand);
    }
    if first < from { return Err(LoanModuleError::InvalidCommand); }
    let interest = if matches!(terms.interest,InterestMethod::OneTimeFixed{..}|InterestMethod::OneTimePercentage{..}) {
        accrue_interest(&AccrualInput{principal:terms.approved_principal,method:terms.interest.clone(),start:from,end_exclusive:from,
            carry:Decimal::ZERO,currency:terms.currency.clone(),rounding:terms.rounding})?.posted_interest
    } else { Decimal::ZERO };
    let schedule = project_schedule(&ScheduleInput { terms:terms.clone(),principal:terms.approved_principal,due_interest:interest,
        carry:Decimal::ZERO,accrual_start:from,first_due_date:first,overrides:vec![] })?;
    Ok(serde_json::json!({"basis":"FULL_APPROVED_FUNDING_PROJECTION","currency":terms.currency,"schedule":schedule_value(schedule)}))
}
pub async fn loan_schedule_preview(tx: &DatabaseTransaction, actor: &LoanActorScope, loan: Uuid, first: NaiveDate) -> LoanResult<serde_json::Value> {
    if actor.permission != LoanPermission::Read { return Err(LoanModuleError::Forbidden); }
    access::entitled(tx,actor).await?;
    let account = store::account(tx,actor.tenant_id,loan).await?;
    access::employee(tx,actor,account.employee_id).await?;
    let today = crate::lifecycle::today(tx,actor.tenant_id).await?;
    if first < today { return Err(LoanModuleError::InvalidCommand); }
    let version = store::terms(tx,&account).await?;
    let mut terms: LoanTerms = serde_json::from_value(version.terms)?;
    let arrangement = store::one(tx,"SELECT monthly_amount::text AS amount,recovery_mode FROM loan_schedule_version WHERE tenant_id=$1 AND loan_id=$2 AND effective_from<=$3 ORDER BY version DESC LIMIT 1",
        vec![actor.tenant_id.into(),loan.into(),today.into()]).await?;
    if let Some(row) = arrangement { terms.monthly_amount=store::decimal(&row,"amount")?; }
    let exit = store::one(tx,"SELECT decision FROM loan_exit_arrangement WHERE tenant_id=$1 AND loan_id=$2 ORDER BY CASE WHEN origin_kind='FNF' THEN 0 ELSE 1 END,created_at DESC,id DESC LIMIT 1",
        vec![actor.tenant_id.into(),loan.into()]).await?;
    if let Some(row) = exit {
        let decision: crate::ExitLoanDecision = serde_json::from_value(row.try_get("","decision")?)?;
        if let Some(continuation) = decision.continuing { terms.monthly_amount=continuation.monthly_amount; terms.recovery=kabipay_loans_domain::RecoveryMode::External; }
    }
    let (principal,interest) = store::balances(tx,actor.tenant_id,loan).await?;
    let earned = accrual::calculate(tx,actor.tenant_id,loan,&terms,principal,account.accrued_through,today,account.rounding_carry).await?;
    let schedule = project_schedule(&ScheduleInput { terms:terms.clone(),principal,due_interest:interest.checked_add(earned.amount).ok_or(LoanModuleError::InvalidCommand)?,
        carry:earned.carry,accrual_start:today,first_due_date:first,overrides:arrangement::overrides(tx,actor.tenant_id,loan).await? })?;
    Ok(serde_json::json!({"basis":"ACTUAL_POSTED_BALANCE_PROJECTION","accountVersion":account.version,"currency":terms.currency,"schedule":schedule_value(schedule)}))
}
fn schedule_value(schedule: kabipay_loans_domain::LoanSchedule) -> serde_json::Value {
    let items = schedule.items.into_iter().map(|item|serde_json::json!({"dueDate":item.due_date,
        "principal":item.principal.to_string(),"interest":item.interest.to_string(),"total":item.total.to_string(),
        "principalAfter":item.principal_after.to_string(),"interestAfter":item.interest_after.to_string()})).collect::<Vec<_>>();
    serde_json::json!({"items":items,"residualPrincipal":schedule.residual_principal.to_string(),
        "residualInterest":schedule.residual_interest.to_string(),"carry":schedule.carry.to_string()})
}
