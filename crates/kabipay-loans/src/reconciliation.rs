//! Recalculate an external reversal without changing any issued payroll/FNF evidence.
use crate::{
    access, accrual, lifecycle, repository as store, LoanActorScope, LoanModuleError,
    LoanPermission, LoanResult,
};
use chrono::NaiveDate;
use kabipay_loans_domain::{
    accrue_interest, allocate_repayment, AccrualInput, AllocationInput, InterestMethod, LoanTerms,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use serde::Serialize;
use uuid::Uuid;

#[derive(Clone, Debug, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanCorrectionPreview {
    pub loan_id: Uuid,
    pub posting_id: Uuid,
    pub account_version: i64,
    pub occurrence_date: NaiveDate,
    pub posting_date: NaiveDate,
    pub currency: String,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub current_principal: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub current_interest: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub corrected_principal: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub corrected_interest: Decimal,
    pub preserved_payroll_postings: Vec<Uuid>,
    pub(crate) allocations: Vec<serde_json::Value>,
    pub review_fingerprint: String,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub(crate) carry: Decimal,
}

fn add(left: Decimal, right: Decimal) -> LoanResult<Decimal> {
    left.checked_add(right)
        .ok_or(LoanModuleError::InvalidCommand)
}

/// The actor must have correction authority. The preview is bound to the locked account,
/// source, calculation and current company date; it is recalculated on approval.
pub async fn preview_loan_reversal(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    posting: Uuid,
) -> LoanResult<LoanCorrectionPreview> {
    if actor.permission != LoanPermission::Correct {
        return Err(LoanModuleError::Forbidden);
    }
    access::entitled(tx, actor).await?;
    store::synchronize(tx, actor.tenant_id).await?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    access::employee(tx, actor, account.employee_id).await?;
    let original = store::one(tx, "SELECT source_kind,value_date FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND id=$3 AND NOT EXISTS(SELECT 1 FROM loan_posting r WHERE r.tenant_id=$1 AND r.loan_id=$2 AND r.reversal_of=$3)", vec![actor.tenant_id.into(),loan.into(),posting.into()]).await?.ok_or(LoanModuleError::SourceAlreadyPosted)?;
    let source: String = original.try_get("", "source_kind")?;
    if !matches!(
        source.as_str(),
        "EXTERNAL_DISBURSEMENT" | "EXTERNAL_RECEIPT"
    ) {
        return Err(LoanModuleError::Forbidden);
    }
    // Credit reversal/refund has separate financial authority and is outside this command.
    if store::one(
        tx,
        "SELECT id FROM loan_unapplied_credit WHERE tenant_id=$1 AND loan_id=$2 LIMIT 1",
        vec![actor.tenant_id.into(), loan.into()],
    )
    .await?
    .is_some()
    {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    let date = lifecycle::today(tx, actor.tenant_id).await?;
    if account.accrued_through.is_some_and(|d| d > date) {
        return Err(LoanModuleError::InvalidCommand);
    }
    let rows = store::all(tx, "SELECT p.id,p.kind,p.source_kind,p.value_date,p.amount::text AS amount,p.principal_delta::text AS principal,p.interest_delta::text AS interest,t.terms FROM loan_posting p JOIN loan_ledger_entry l ON l.tenant_id=p.tenant_id AND l.loan_id=p.loan_id AND l.posting_id=p.id JOIN loan_terms_version t ON t.tenant_id=p.tenant_id AND t.loan_id=p.loan_id AND t.id=p.terms_version_id WHERE p.tenant_id=$1 AND p.loan_id=$2 AND p.id<>$3 AND p.kind IN ('DISBURSEMENT','REPAYMENT') AND NOT EXISTS(SELECT 1 FROM loan_posting r WHERE r.tenant_id=p.tenant_id AND r.loan_id=p.loan_id AND r.reversal_of=p.id) ORDER BY p.value_date,l.sequence", vec![actor.tenant_id.into(),loan.into(),posting.into()]).await?;
    let mut principal = Decimal::ZERO;
    let mut interest = Decimal::ZERO;
    let mut carry = Decimal::ZERO;
    let mut cursor = None;
    let mut frozen = vec![];
    let mut allocations = vec![];
    for row in rows {
        let event_date: NaiveDate = row.try_get("", "value_date")?;
        if event_date > date {
            return Err(LoanModuleError::InvalidCommand);
        }
        let terms: LoanTerms = serde_json::from_value(row.try_get("", "terms")?)?;
        terms.validate()?;
        if terms.currency.code != account.currency
            || i32::try_from(terms.currency.minor_units).ok() != Some(account.minor_units)
        {
            return Err(LoanModuleError::InvalidCommand);
        }
        let earned = accrual::calculate(
            tx,
            actor.tenant_id,
            loan,
            &terms,
            principal,
            cursor,
            event_date,
            carry,
        )
        .await?;
        interest = add(interest, earned.amount)?;
        carry = earned.carry;
        let amount = store::decimal(&row, "amount")?;
        let kind: String = row.try_get("", "kind")?;
        let source: String = row.try_get("", "source_kind")?;
        if kind == "DISBURSEMENT" {
            principal = add(principal, amount)?;
            if matches!(
                terms.interest,
                InterestMethod::OneTimeFixed { .. } | InterestMethod::OneTimePercentage { .. }
            ) {
                let charge = accrue_interest(&AccrualInput {
                    principal: amount,
                    method: terms.interest.clone(),
                    start: event_date,
                    end_exclusive: event_date,
                    carry,
                    currency: terms.currency.clone(),
                    rounding: terms.rounding,
                })?;
                interest = add(interest, charge.posted_interest)?;
                carry = charge.carry;
            }
        } else if matches!(source.as_str(), "PAYROLL" | "FNF" | "EXTERNAL_RECEIPT") {
            let posting_id = row.try_get::<Uuid>("", "id")?;
            if matches!(source.as_str(), "PAYROLL" | "FNF") {
                // Preserve issued source evidence; reclassify only the current loan ledger.
                frozen.push(posting_id);
            }
            let allocation = allocate_repayment(&AllocationInput {
                payment: amount,
                principal,
                interest,
                order: terms.allocation,
                currency: terms.currency,
            })?;
            if allocation.excess_credit > Decimal::ZERO {
                return Err(LoanModuleError::HistoricalReconciliationRequired);
            }
            allocations.push(serde_json::json!({
                "postingId": posting_id,
                "sourceKind": source,
                "originalPrincipal": store::decimal(&row, "principal")?.to_string(),
                "originalInterest": store::decimal(&row, "interest")?.to_string(),
                "correctedPrincipal": (-allocation.principal).to_string(),
                "correctedInterest": (-allocation.interest).to_string(),
            }));
            principal = add(principal, -allocation.principal)?;
            interest = add(interest, -allocation.interest)?;
        } else {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        if principal < Decimal::ZERO || interest < Decimal::ZERO {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        cursor = Some(event_date);
    }
    let terms = store::terms(tx, &account).await?;
    let terms: LoanTerms = serde_json::from_value(terms.terms)?;
    let earned = accrual::calculate(
        tx,
        actor.tenant_id,
        loan,
        &terms,
        principal,
        cursor,
        date,
        carry,
    )
    .await?;
    interest = add(interest, earned.amount)?;
    let (current_principal, current_interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let mut result = LoanCorrectionPreview {
        loan_id: loan,
        posting_id: posting,
        account_version: account.version,
        occurrence_date: original.try_get("", "value_date")?,
        posting_date: date,
        currency: account.currency,
        current_principal,
        current_interest,
        corrected_principal: principal,
        corrected_interest: interest,
        preserved_payroll_postings: frozen,
        allocations,
        review_fingerprint: String::new(),
        carry: earned.carry,
    };
    result.review_fingerprint = crate::command_hash(&(actor.tenant_id, actor.user_id, &result))?;
    Ok(result)
}
