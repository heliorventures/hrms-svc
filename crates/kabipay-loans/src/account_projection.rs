//! Dated arrangements use their contractual anchors, never finite forecast rows.
use crate::{
    access, arrangement, lifecycle, repository as store, LoanActorScope, LoanModuleError,
    LoanResult,
};
use chrono::{Datelike, Months, NaiveDate};
use kabipay_loans_domain::{
    accrue_calendar_interest, accrue_interest, project_schedule, AccrualInput, InterestMethod,
    LoanSchedule, LoanTerms, PeriodOverride, RecoveryMode, ScheduleInput,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub(crate) struct DatedArrangement {
    pub effective_from: NaiveDate,
    pub first_due_date: NaiveDate,
    pub monthly_amount: Decimal,
    pub recovery: RecoveryMode,
}

pub(crate) struct AccountProjection {
    pub terms: LoanTerms,
    pub account_version: i64,
    pub basis: &'static str,
    pub funding_assumption_date: Option<NaiveDate>,
    pub schedule: LoanSchedule,
}

pub(crate) async fn calculate_account(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    proposed: Option<DatedArrangement>,
    override_: Option<PeriodOverride>,
) -> LoanResult<AccountProjection> {
    let account = store::account(tx, actor.tenant_id, loan).await?;
    access::employee(tx, actor, account.employee_id).await?;
    let today = lifecycle::today(tx, actor.tenant_id).await?;
    let version = store::terms(tx, &account).await?;
    let terms: LoanTerms = serde_json::from_value(version.terms)?;
    let mut overrides = arrangement::overrides(tx, actor.tenant_id, loan).await?;
    if let Some(value) = override_ {
        overrides.retain(|old| old.period != value.period);
        overrides.push(value);
    }
    let pauses = overrides
        .iter()
        .filter(|value| value.pause_interest)
        .map(|value| value.period)
        .collect::<Vec<_>>();
    let (mut principal, mut interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let unfunded = account.funding_state == "UNFUNDED";
    let funding_assumption_date = unfunded.then_some(today.max(version.effective_from));
    let projection_start = funding_assumption_date.unwrap_or(today);
    let mut carry = account.rounding_carry;
    if unfunded {
        // This is a clearly labelled funding assumption, never an outstanding ledger balance.
        principal = terms.approved_principal;
        carry = Decimal::ZERO;
        if matches!(
            terms.interest,
            InterestMethod::OneTimeFixed { .. } | InterestMethod::OneTimePercentage { .. }
        ) {
            interest = accrue_interest(&AccrualInput {
                principal,
                method: terms.interest.clone(),
                start: projection_start,
                end_exclusive: projection_start,
                carry,
                currency: terms.currency.clone(),
                rounding: terms.rounding,
            })?
            .posted_interest;
        }
    } else {
        if account.accrued_through.is_some_and(|start| start > today) {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        let earned = accrue_calendar_interest(
            &AccrualInput {
                principal,
                method: terms.interest.clone(),
                start: account.accrued_through.unwrap_or(today),
                end_exclusive: today,
                carry,
                currency: terms.currency.clone(),
                rounding: terms.rounding,
            },
            &pauses,
        )?;
        interest = interest
            .checked_add(earned.posted_interest)
            .ok_or(LoanModuleError::InvalidCommand)?;
        carry = earned.carry;
    }

    let rows = store::all(tx, "SELECT s.effective_from,s.monthly_amount::text AS amount,s.recovery_mode,COALESCE(s.first_due_date,(SELECT MIN(i.due_date) FROM loan_schedule_item i WHERE i.tenant_id=s.tenant_id AND i.loan_id=s.loan_id AND i.schedule_version_id=s.id)) AS first_due_date FROM loan_schedule_version s WHERE s.tenant_id=$1 AND s.loan_id=$2 ORDER BY s.version",
        vec![actor.tenant_id.into(), loan.into()]).await?;
    let mut versions = Vec::with_capacity(rows.len() + 1);
    for row in rows {
        let mode: String = row.try_get("", "recovery_mode")?;
        let recovery = match mode.as_str() {
            "PAYROLL" => RecoveryMode::Payroll,
            "EXTERNAL" => RecoveryMode::External,
            "MIXED" => RecoveryMode::Mixed,
            _ => return Err(LoanModuleError::InvalidCommand),
        };
        versions.push((
            row.try_get::<NaiveDate>("", "effective_from")?,
            row.try_get::<Option<NaiveDate>>("", "first_due_date")?,
            store::decimal(&row, "amount")?,
            recovery,
        ));
    }
    if let Some(value) = proposed {
        versions.push((
            value.effective_from,
            Some(value.first_due_date),
            value.monthly_amount,
            value.recovery,
        ));
    }
    let exit = store::one(tx, "SELECT decision FROM loan_exit_arrangement WHERE tenant_id=$1 AND loan_id=$2 ORDER BY CASE WHEN origin_kind='FNF' THEN 0 ELSE 1 END,created_at DESC,id DESC LIMIT 1",
        vec![actor.tenant_id.into(), loan.into()]).await?;
    if let Some(row) = exit {
        let decision: crate::ExitLoanDecision =
            serde_json::from_value(row.try_get("", "decision")?)?;
        if let Some(value) = decision.continuing {
            versions.push((
                today.min(value.first_payment_date),
                Some(value.first_payment_date),
                value.monthly_amount,
                RecoveryMode::External,
            ));
        }
    }
    // A historical missing anchor must not prevent explicit HR remediation. Only
    // versions that can still govern today or a future date need usable evidence.
    let mut arrangements = Vec::new();
    for (index, &(from, first, monthly_amount, recovery)) in versions.iter().enumerate() {
        if versions[index + 1..]
            .iter()
            .any(|later| later.0 <= from.max(today))
        {
            continue;
        }
        arrangements.push(DatedArrangement {
            effective_from: from,
            first_due_date: first.ok_or(LoanModuleError::RepaymentReviewRequired)?,
            monthly_amount,
            recovery,
        });
    }
    let mut dates = BTreeSet::new();
    // An instalment already due in the valuation month remains recoverable on
    // the assumed funding date; accrual still begins at projection_start.
    let due_window_start = projection_start
        .with_day(1)
        .ok_or(LoanModuleError::InvalidCommand)?;
    for value in &arrangements {
        for offset in 0..terms.max_instalments {
            let date = value
                .first_due_date
                .checked_add_months(Months::new(offset))
                .ok_or(LoanModuleError::InvalidCommand)?;
            if date >= due_window_start && date >= value.effective_from {
                dates.insert(date);
            }
        }
    }
    let recovered_rows = store::all(tx, "SELECT (CASE WHEN p.kind='REVERSAL' THEN original.calculation_evidence ELSE p.calculation_evidence END)->>'periodStart' AS period,COALESCE(-SUM(p.principal_delta+p.interest_delta),0)::text AS recovered FROM loan_posting p LEFT JOIN loan_posting original ON original.tenant_id=p.tenant_id AND original.loan_id=p.loan_id AND original.id=p.reversal_of WHERE p.tenant_id=$1 AND p.loan_id=$2 AND (p.source_kind='PAYROLL' OR (p.kind='REVERSAL' AND original.source_kind='PAYROLL')) GROUP BY period",
        vec![actor.tenant_id.into(), loan.into()]).await?;
    let mut recovered = BTreeMap::new();
    for row in recovered_rows {
        if let Some(period) = row.try_get::<Option<String>>("", "period")? {
            recovered.insert(period, store::decimal(&row, "recovered")?);
        }
    }
    let mut items = Vec::new();
    let mut start = projection_start;
    for date in dates {
        if principal.is_zero() && interest.is_zero() {
            break;
        }
        let Some(active) = arrangements
            .iter()
            .rfind(|value| value.effective_from <= date.max(projection_start))
        else {
            continue;
        };
        let offset = (date.year() - active.first_due_date.year()) * 12 + date.month() as i32
            - active.first_due_date.month() as i32;
        if offset < 0
            || offset >= terms.max_instalments as i32
            || active
                .first_due_date
                .checked_add_months(Months::new(offset as u32))
                != Some(date)
        {
            continue;
        }
        let period = date.with_day(1).ok_or(LoanModuleError::InvalidCommand)?;
        let mut amount = overrides
            .iter()
            .find(|value| value.period == period)
            .map_or(Some(active.monthly_amount), |value| value.amount)
            .unwrap_or(Decimal::ZERO);
        if active.recovery != RecoveryMode::External {
            amount = amount
                .checked_sub(
                    recovered
                        .get(&period.to_string())
                        .copied()
                        .unwrap_or(Decimal::ZERO),
                )
                .ok_or(LoanModuleError::InvalidCommand)?
                .max(Decimal::ZERO);
        }
        let mut step_terms = terms.clone();
        step_terms.monthly_amount = active.monthly_amount;
        step_terms.recovery = active.recovery;
        step_terms.max_instalments = 1;
        step_terms.allow_residual = true;
        let mut step_overrides = overrides.clone();
        step_overrides.retain(|value| value.period != period);
        step_overrides.push(PeriodOverride {
            period,
            amount: if amount.is_zero() { None } else { Some(amount) },
            pause_interest: pauses.contains(&period),
        });
        let value_date = date.max(start);
        let mut schedule = project_schedule(&ScheduleInput {
            terms: step_terms,
            principal,
            due_interest: interest,
            carry,
            accrual_start: start,
            first_due_date: value_date,
            overrides: step_overrides,
        })?;
        // A current-period overdue instalment is recoverable now. Preserve its
        // contractual date separately from the projection's valuation date.
        for item in &mut schedule.items {
            item.due_date = date;
        }
        principal = schedule.residual_principal;
        interest = schedule.residual_interest;
        carry = schedule.carry;
        if active.recovery != RecoveryMode::External {
            let paid = schedule
                .items
                .iter()
                .try_fold(Decimal::ZERO, |total, item| {
                    total
                        .checked_add(item.total)
                        .ok_or(LoanModuleError::InvalidCommand)
                })?;
            let prior = recovered.entry(period.to_string()).or_insert(Decimal::ZERO);
            *prior = prior
                .checked_add(paid)
                .ok_or(LoanModuleError::InvalidCommand)?;
        }
        items.extend(schedule.items);
        start = value_date;
    }
    if (!principal.is_zero() || !interest.is_zero()) && !terms.allow_residual {
        return Err(LoanModuleError::RepaymentReviewRequired);
    }
    Ok(AccountProjection {
        terms,
        account_version: account.version,
        funding_assumption_date,
        basis: if unfunded {
            "APPROVED_UNFUNDED_BALANCE_PROJECTION"
        } else {
            "ACTUAL_POSTED_BALANCE_PROJECTION"
        },
        schedule: LoanSchedule {
            items,
            residual_principal: principal,
            residual_interest: interest,
            carry,
        },
    })
}
