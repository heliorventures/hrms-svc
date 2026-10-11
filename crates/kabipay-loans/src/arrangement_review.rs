//! Shared read-only validation for proposed and persisted repayment changes.
use crate::{
    access, lifecycle, projection, repository as store, LoanActorScope, LoanCommand,
    LoanModuleError, LoanPermission, LoanResult,
};
use chrono::Datelike;
use kabipay_db_entities::tenant::d0103_employee_loans::loan_account;
use kabipay_loans_domain::{LoanPolicy, LoanTerms, PeriodOverride};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) struct ReviewedChange {
    pub account: loan_account::Model,
    pub terms: LoanTerms,
    pub terms_id: Uuid,
}

pub(crate) async fn validate_change(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    input: &LoanCommand,
) -> LoanResult<ReviewedChange> {
    if actor.permission != LoanPermission::Manage {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    let (loan, reason) = match input {
        LoanCommand::SetDeduction {
            loan_id, reason, ..
        }
        | LoanCommand::SetPeriodOverride {
            loan_id, reason, ..
        } => (*loan_id, reason),
        _ => return Err(LoanModuleError::InvalidCommand),
    };
    lifecycle::text(reason, 2000)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    lifecycle::checked_version(account.version, expected)?;
    access::employee(tx, actor, account.employee_id).await?;
    let today = lifecycle::today(tx, actor.tenant_id).await?;
    let version = store::terms(tx, &account).await?;
    let mut terms: LoanTerms = serde_json::from_value(version.terms)?;
    let policy = store::policy(tx, actor.tenant_id, version.policy_version_id).await?;
    let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
    match input {
        LoanCommand::SetDeduction {
            effective_from,
            first_due_date,
            amount,
            recovery,
            ..
        } => {
            if account.state != "OPEN" || *effective_from < today || first_due_date < effective_from
            {
                return Err(LoanModuleError::InvalidCommand);
            }
            terms.monthly_amount = *amount;
            terms.recovery = *recovery;
            rules.validate_terms(&terms)?;
        }
        LoanCommand::SetPeriodOverride {
            period_start,
            amount,
            pause_interest,
            ..
        } => {
            if account.state != "OPEN"
                || period_start.day() != 1
                || *period_start < today.with_day(1).ok_or(LoanModuleError::InvalidCommand)?
            {
                return Err(LoanModuleError::HistoricalReconciliationRequired);
            }
            let previous = store::one(tx, "SELECT accrual_treatment FROM loan_period_override WHERE tenant_id=$1 AND loan_id=$2 AND period_start=$3", vec![actor.tenant_id.into(),loan.into(),(*period_start).into()]).await?;
            let was_paused = previous
                .map(|row| row.try_get::<String>("", "accrual_treatment"))
                .transpose()?
                .is_some_and(|value| value == "PAUSE");
            if *pause_interest != was_paused
                && account
                    .accrued_through
                    .is_some_and(|date| date > *period_start)
            {
                return Err(LoanModuleError::HistoricalReconciliationRequired);
            }
            rules.validate_terms(&terms)?;
            if amount.is_some() && !rules.recovery.allow_overrides
                || amount.is_none() && !rules.recovery.allow_skips
                || *pause_interest && !rules.recovery.allow_accrual_pause
            {
                return Err(LoanModuleError::Forbidden);
            }
            if let Some(amount) = amount {
                terms.currency.validate_amount(*amount)?;
                if *amount <= Decimal::ZERO {
                    return Err(LoanModuleError::InvalidCommand);
                }
            }
            if store::one(tx,"SELECT id FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND source_kind='PAYROLL' AND calculation_evidence->>'periodStart'=$3::date::text LIMIT 1",vec![actor.tenant_id.into(),loan.into(),(*period_start).into()]).await?.is_some() {
                return Err(LoanModuleError::VersionConflict);
            }
        }
        _ => return Err(LoanModuleError::InvalidCommand),
    }
    Ok(ReviewedChange {
        account,
        terms,
        terms_id: version.id,
    })
}

/// Produces the proposed financial schedule without writing an arrangement, override,
/// command receipt, account revision, or ledger entry. The same checks protect writes.
pub async fn preview_arrangement_change(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    input: &LoanCommand,
) -> LoanResult<serde_json::Value> {
    if actor.permission != LoanPermission::Manage {
        return Err(LoanModuleError::Forbidden);
    }
    store::synchronize(tx, actor.tenant_id).await?;
    let change = validate_change(tx, actor, expected, input).await?;
    let (proposed, override_) = match input {
        LoanCommand::SetDeduction {
            effective_from,
            first_due_date,
            amount,
            recovery,
            ..
        } => (
            Some(projection::DatedArrangement {
                effective_from: *effective_from,
                first_due_date: *first_due_date,
                monthly_amount: *amount,
                recovery: *recovery,
            }),
            None,
        ),
        LoanCommand::SetPeriodOverride {
            period_start,
            amount,
            pause_interest,
            ..
        } => (
            None,
            Some(PeriodOverride {
                period: *period_start,
                amount: *amount,
                pause_interest: *pause_interest,
            }),
        ),
        _ => return Err(LoanModuleError::InvalidCommand),
    };
    projection::project_account(tx, actor, change.account.id, None, proposed, override_).await
}
