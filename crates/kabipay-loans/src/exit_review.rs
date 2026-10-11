//! Loan-owned exposure. Resignation decisions are forecasts, never repayments.
use crate::{
    access, accrual, lifecycle, posting, repository as store, LoanActorScope, LoanModuleError,
    LoanPermission, LoanResult,
};
use chrono::NaiveDate;
use kabipay_loans_domain::{Currency, LoanPolicy, LoanTerms};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct ExitLoanBalance {
    pub loan_id: Uuid,
    pub loan_number: String,
    pub currency: Currency,
    pub account_version: i64,
    pub terms_version_id: Uuid,
    pub policy_version_id: Uuid,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub principal: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub posted_interest: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub earned_unposted_interest: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub forecast_interest: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub forecast_payoff: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub unfunded_commitment: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub unapplied_credit: Decimal,
    pub fnf_allowed: bool,
    pub continuing_allowed: bool,
    pub policy_review_reference: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanExitExposure {
    pub employee_id: Uuid,
    pub as_of: NaiveDate,
    pub settlement_date: NaiveDate,
    pub employee_revision: i64,
    pub loans: Vec<ExitLoanBalance>,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ContinuingRepayment {
    pub first_payment_date: NaiveDate,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub monthly_amount: Decimal,
    pub follow_up_user_id: Uuid,
    pub agreement_reference: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitLoanDecision {
    pub loan_id: Uuid,
    pub final_payroll_cycle_id: Option<Uuid>,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub proposed_payroll_recovery: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub proposed_fnf_recovery: Decimal,
    pub continuing: Option<ContinuingRepayment>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitFinancialDecision {
    pub reason: String,
    pub loans: Vec<ExitLoanDecision>,
}

pub async fn preview_exit_exposure(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    employee: Uuid,
    settlement: NaiveDate,
) -> LoanResult<LoanExitExposure> {
    if !matches!(
        actor.permission,
        LoanPermission::ExitReview | LoanPermission::FnfRecovery
    ) {
        return Err(LoanModuleError::Forbidden);
    }
    access::company(actor)?;
    access::employee(tx, actor, employee).await?;
    store::synchronize(tx, actor.tenant_id).await?;
    if actor.permission == LoanPermission::FnfRecovery {
        // HR's existing FNF flow remains available without Loans only when no
        // outstanding Loan account or employee credit is being hidden.
        crate::payroll_loan_state(tx, actor.tenant_id).await?;
    } else {
        access::entitled(tx, actor).await?;
    }
    let today = lifecycle::today(tx, actor.tenant_id).await?;
    if settlement < today && actor.permission != LoanPermission::FnfRecovery {
        return Err(LoanModuleError::InvalidCommand);
    }
    let revision = store::one(
        tx,
        "SELECT revision FROM loan_employee_state WHERE tenant_id=$1 AND employee_id=$2",
        vec![actor.tenant_id.into(), employee.into()],
    )
    .await?
    .map(|row| row.try_get::<i64>("", "revision"))
    .transpose()?
    .unwrap_or(0);
    let rows=store::all(tx,"SELECT id FROM loan_account WHERE tenant_id=$1 AND employee_id=$2 AND (state='OPEN' OR EXISTS(SELECT 1 FROM loan_unapplied_credit c WHERE c.tenant_id=loan_account.tenant_id AND c.loan_id=loan_account.id AND c.remaining_amount>0)) ORDER BY created_at,id",vec![actor.tenant_id.into(),employee.into()]).await?;
    let mut lines = vec![];
    for row in rows {
        let account = store::account(tx, actor.tenant_id, row.try_get("", "id")?).await?;
        if settlement < today && store::one(tx,
            "SELECT id FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND value_date>$3 LIMIT 1",
            vec![actor.tenant_id.into(),account.id.into(),settlement.into()]).await?.is_some() {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        let version = store::terms(tx, &account).await?;
        let terms: LoanTerms = serde_json::from_value(version.terms)?;
        let policy = store::policy(tx, actor.tenant_id, version.policy_version_id).await?;
        let rules: LoanPolicy = serde_json::from_value(policy.rules)?;
        rules.validate_terms(&terms)?;
        let (principal, interest) = store::balances(tx, actor.tenant_id, account.id).await?;
        let earned = accrual::calculate(
            tx,
            actor.tenant_id,
            account.id,
            &terms,
            principal,
            account.accrued_through,
            today,
            account.rounding_carry,
        )
        .await?;
        let projected = accrual::calculate(
            tx,
            actor.tenant_id,
            account.id,
            &terms,
            principal,
            account.accrued_through,
            settlement,
            account.rounding_carry,
        )
        .await?;
        let funding = posting::net_funding(tx, actor.tenant_id, account.id).await?;
        let credit=store::one(tx,"SELECT COALESCE(SUM(remaining_amount),0)::text AS amount FROM loan_unapplied_credit WHERE tenant_id=$1 AND loan_id=$2",vec![actor.tenant_id.into(),account.id.into()]).await?.ok_or(LoanModuleError::NotFound)?;
        lines.push(ExitLoanBalance {
            loan_id: account.id,
            loan_number: account.loan_number,
            currency: terms.currency,
            account_version: account.version,
            terms_version_id: version.id,
            policy_version_id: version.policy_version_id,
            principal,
            posted_interest: interest,
            earned_unposted_interest: earned.amount,
            forecast_interest: projected.amount,
            forecast_payoff: principal
                .checked_add(interest)
                .and_then(|value| value.checked_add(projected.amount))
                .ok_or(LoanModuleError::InvalidCommand)?,
            unfunded_commitment: if account.funding_state == "CANCELLED" {
                Decimal::ZERO
            } else {
                account
                    .approved_principal
                    .checked_sub(funding)
                    .ok_or(LoanModuleError::InvalidCommand)?
            },
            unapplied_credit: store::decimal(&credit, "amount")?,
            fnf_allowed: rules.exit_recovery.allow_fnf,
            continuing_allowed: rules.exit_recovery.allow_continuing,
            policy_review_reference: rules.exit_recovery.review_reference,
        });
    }
    let fingerprint = crate::command_hash(&(employee, today, settlement, revision, &lines))?;
    Ok(LoanExitExposure {
        employee_id: employee,
        as_of: today,
        settlement_date: settlement,
        employee_revision: revision,
        loans: lines,
        fingerprint,
    })
}

pub fn validate_exit_decision(
    exposure: &LoanExitExposure,
    decision: &ExitFinancialDecision,
) -> LoanResult<()> {
    lifecycle::text(&decision.reason, 2000)?;
    if decision.loans.len() != exposure.loans.len() {
        return Err(LoanModuleError::InvalidCommand);
    }
    let mut seen = std::collections::HashSet::new();
    for selected in &decision.loans {
        if !seen.insert(selected.loan_id) {
            return Err(LoanModuleError::InvalidCommand);
        }
        let balance = exposure
            .loans
            .iter()
            .find(|line| line.loan_id == selected.loan_id)
            .ok_or(LoanModuleError::InvalidCommand)?;
        balance
            .currency
            .validate_amount(selected.proposed_fnf_recovery)?;
        balance
            .currency
            .validate_amount(selected.proposed_payroll_recovery)?;
        if (selected.proposed_fnf_recovery > Decimal::ZERO && !balance.fnf_allowed)
            || (selected.proposed_payroll_recovery > Decimal::ZERO
                && !selected
                    .final_payroll_cycle_id
                    .is_some_and(|id| !id.is_nil()))
        {
            return Err(LoanModuleError::PolicyUnavailable);
        }
        let planned = selected
            .proposed_fnf_recovery
            .checked_add(selected.proposed_payroll_recovery)
            .ok_or(LoanModuleError::InvalidCommand)?;
        if planned > balance.forecast_payoff {
            return Err(LoanModuleError::ExcessPayment);
        }
        if let Some(continuing) = &selected.continuing {
            balance
                .currency
                .validate_amount(continuing.monthly_amount)?;
            lifecycle::text(&continuing.agreement_reference, 500)?;
            if !balance.continuing_allowed
                || continuing.monthly_amount <= Decimal::ZERO
                || continuing.follow_up_user_id.is_nil()
                || continuing.first_payment_date < exposure.settlement_date
            {
                return Err(LoanModuleError::PolicyUnavailable);
            }
        } else if planned < balance.forecast_payoff {
            return Err(LoanModuleError::InvalidCommand);
        }
    }
    Ok(())
}
