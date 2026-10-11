//! Recalculate an external reversal without changing any issued payroll/FNF evidence.
use crate::{
    access, lifecycle, repository as store, LoanActorScope, LoanModuleError, LoanPermission,
    LoanResult,
};
use chrono::NaiveDate;
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
    let original = store::one(tx, "SELECT source_kind,COALESCE((SELECT value_date FROM loan_receipt r WHERE r.tenant_id=p.tenant_id AND r.loan_id=p.loan_id AND r.id=p.source_id AND p.source_kind='EXTERNAL_RECEIPT'),(SELECT value_date FROM loan_disbursement d WHERE d.tenant_id=p.tenant_id AND d.loan_id=p.loan_id AND d.id=p.source_id AND p.source_kind='EXTERNAL_DISBURSEMENT'),p.value_date) AS value_date FROM loan_posting p WHERE p.tenant_id=$1 AND p.loan_id=$2 AND p.id=$3 AND NOT EXISTS(SELECT 1 FROM loan_posting r WHERE r.tenant_id=$1 AND r.loan_id=$2 AND r.reversal_of=$3)", vec![actor.tenant_id.into(),loan.into(),posting.into()]).await?.ok_or(LoanModuleError::SourceAlreadyPosted)?;
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
    let replay =
        crate::replay::calculate(tx, actor.tenant_id, loan, Some(posting), None, date).await?;
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
        corrected_principal: replay.principal,
        corrected_interest: replay.interest,
        preserved_payroll_postings: replay.frozen,
        allocations: replay.allocations,
        review_fingerprint: String::new(),
        carry: replay.carry,
    };
    result.review_fingerprint = crate::command_hash(&(actor.tenant_id, actor.user_id, &result))?;
    Ok(result)
}
