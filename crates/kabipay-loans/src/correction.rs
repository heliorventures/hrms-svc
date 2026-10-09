use crate::{
    access, lifecycle,
    posting::{self, Posting},
    repository as store, LoanActorScope, LoanCommandResult, LoanModuleError, LoanResult,
};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

/// Reverse an external event only if no downstream financial event depends on it.
/// Historical downstream chains require a reviewed reconciliation; never silently rewrite them.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn reverse(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    loan: Uuid,
    id: Uuid,
    reason: &str,
    reference: &str,
    review_fingerprint: Option<&str>,
) -> LoanResult<LoanCommandResult> {
    lifecycle::text(reason, 2000)?;
    lifecycle::text(reference, 500)?;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    lifecycle::checked_version(account.version, expected)?;
    access::employee(tx, actor, account.employee_id).await?;
    let original=store::one(tx,"SELECT p.*,p.amount::text AS amount_text,p.principal_delta::text AS principal_text,p.interest_delta::text AS interest_text,l.sequence FROM loan_posting p JOIN loan_ledger_entry l ON l.tenant_id=p.tenant_id AND l.loan_id=p.loan_id AND l.posting_id=p.id WHERE p.tenant_id=$1 AND p.loan_id=$2 AND p.id=$3",vec![actor.tenant_id.into(),loan.into(),id.into()]).await?.ok_or(LoanModuleError::NotFound)?;
    let source_kind: String = original.try_get("", "source_kind")?;
    let source: Uuid = original.try_get("", "source_id")?;
    let sequence: i64 = original.try_get("", "sequence")?;
    if !matches!(
        source_kind.as_str(),
        "EXTERNAL_DISBURSEMENT" | "EXTERNAL_RECEIPT"
    ) {
        return Err(LoanModuleError::Forbidden);
    }
    if store::one(
        tx,
        "SELECT id FROM loan_posting WHERE tenant_id=$1 AND loan_id=$2 AND reversal_of=$3",
        vec![actor.tenant_id.into(), loan.into(), id.into()],
    )
    .await?
    .is_some()
    {
        return Err(LoanModuleError::SourceAlreadyPosted);
    }
    if let Some(fingerprint) = review_fingerprint {
        let review = crate::preview_loan_reversal(tx, actor, loan, id).await?;
        if review.review_fingerprint != fingerprint {
            return Err(LoanModuleError::VersionConflict);
        }
        return reconciled(tx, actor, expected, original, review, reason, reference).await;
    }
    // Reversing held credit needs a separate refund/reallocation review, not erasure.
    if source_kind=="EXTERNAL_RECEIPT" && store::one(tx,"SELECT id FROM loan_unapplied_credit WHERE tenant_id=$1 AND loan_id=$2 AND receipt_id=$3",vec![actor.tenant_id.into(),loan.into(),source.into()]).await?.is_some(){return Err(LoanModuleError::HistoricalReconciliationRequired)}
    let subsequent=store::all(tx,"SELECT p.*,p.amount::text AS amount_text,p.principal_delta::text AS principal_text,p.interest_delta::text AS interest_text,l.sequence FROM loan_posting p JOIN loan_ledger_entry l ON l.tenant_id=p.tenant_id AND l.loan_id=p.loan_id AND l.posting_id=p.id WHERE p.tenant_id=$1 AND p.loan_id=$2 AND l.sequence>$3 ORDER BY l.sequence DESC",vec![actor.tenant_id.into(),loan.into(),sequence.into()]).await?;
    if subsequent.iter().any(|row| {
        row.try_get::<String>("", "source_kind").ok().as_deref() != Some("FUNDING_CHARGE")
            || row.try_get::<Uuid>("", "source_id").ok() != Some(source)
    }) {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    let mut targets = subsequent;
    targets.push(original);
    let mut ids = vec![];
    for target in targets {
        let original_id: Uuid = target.try_get("", "id")?;
        let principal = store::decimal(&target, "principal_text")?;
        let interest = store::decimal(&target, "interest_text")?;
        let (current_principal, current_interest) =
            store::balances(tx, actor.tenant_id, loan).await?;
        if current_principal
            .checked_sub(principal)
            .ok_or(LoanModuleError::InvalidCommand)?
            < Decimal::ZERO
            || current_interest
                .checked_sub(interest)
                .ok_or(LoanModuleError::InvalidCommand)?
                < Decimal::ZERO
        {
            return Err(LoanModuleError::HistoricalReconciliationRequired);
        }
        ids.push(posting::post(tx,actor,Posting{loan,terms:target.try_get("","terms_version_id")?,kind:"REVERSAL",source_kind:"EXTERNAL_CORRECTION",source:original_id,revision:expected,date:lifecycle::today(tx,actor.tenant_id).await?,amount:store::decimal(&target,"amount_text")?,principal:-principal,interest:-interest,reversal:Some(original_id),evidence:serde_json::json!({"reason":reason,"reconciliationReference":reference,"occurrenceDate":target.try_get::<chrono::NaiveDate>("","value_date")?,"classification":"EXTERNAL_EVENT_REVERSAL"})}).await?);
        if matches!(
            target.try_get::<String>("", "source_kind")?.as_str(),
            "FUNDING_CHARGE" | "EXTERNAL_DISBURSEMENT"
        ) {
            let evidence: serde_json::Value = target.try_get("", "calculation_evidence")?;
            let carry: Decimal = serde_json::from_value(
                evidence
                    .get("carryBefore")
                    .cloned()
                    .ok_or(LoanModuleError::InvalidCommand)?,
            )?;
            store::execute(
                tx,
                "UPDATE loan_account SET rounding_carry=$3 WHERE tenant_id=$1 AND id=$2",
                vec![actor.tenant_id.into(), loan.into(), carry.into()],
            )
            .await?;
        }
    }
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    let mut result = lifecycle::result(
        id,
        Some(loan),
        Some(account.employee_id),
        version,
        "REVERSED",
    );
    result.posting_ids = ids;
    Ok(result)
}

async fn reconciled(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    expected: i64,
    original: sea_orm::QueryResult,
    review: crate::LoanCorrectionPreview,
    reason: &str,
    reference: &str,
) -> LoanResult<LoanCommandResult> {
    let loan = review.loan_id;
    let account = store::account(tx, actor.tenant_id, loan).await?;
    let source: Uuid = original.try_get("", "source_id")?;
    let mut targets = store::all(tx, "SELECT p.*,p.amount::text AS amount_text,p.principal_delta::text AS principal_text,p.interest_delta::text AS interest_text FROM loan_posting p WHERE p.tenant_id=$1 AND p.loan_id=$2 AND p.source_kind='FUNDING_CHARGE' AND p.source_id=$3 AND NOT EXISTS(SELECT 1 FROM loan_posting r WHERE r.tenant_id=p.tenant_id AND r.reversal_of=p.id)", vec![actor.tenant_id.into(),loan.into(),source.into()]).await?;
    targets.push(original);
    let mut ids = vec![];
    for target in targets {
        let id: Uuid = target.try_get("", "id")?;
        ids.push(posting::post(tx, actor, Posting { loan, terms: target.try_get("", "terms_version_id")?, kind: "REVERSAL", source_kind: "EXTERNAL_CORRECTION", source: id, revision: expected, date: review.posting_date, amount: store::decimal(&target, "amount_text")?, principal: -store::decimal(&target, "principal_text")?, interest: -store::decimal(&target, "interest_text")?, reversal: Some(id), evidence: serde_json::json!({"classification":"EXTERNAL_EVENT_REVERSAL","occurrenceDate":target.try_get::<chrono::NaiveDate>("","value_date")?,"reason":reason,"reconciliationReference":reference,"reviewFingerprint":review.review_fingerprint}) }).await?);
    }
    let (principal, interest) = store::balances(tx, actor.tenant_id, loan).await?;
    let principal_delta = review
        .corrected_principal
        .checked_sub(principal)
        .ok_or(LoanModuleError::InvalidCommand)?;
    let interest_delta = review
        .corrected_interest
        .checked_sub(interest)
        .ok_or(LoanModuleError::InvalidCommand)?;
    if principal_delta != Decimal::ZERO || interest_delta != Decimal::ZERO {
        let amount = principal_delta
            .abs()
            .checked_add(interest_delta.abs())
            .ok_or(LoanModuleError::InvalidCommand)?;
        ids.push(posting::post(tx, actor, Posting { loan, terms: account.current_terms_id.ok_or(LoanModuleError::InvalidCommand)?, kind: "ADJUSTMENT", source_kind: "HISTORICAL_RECONCILIATION", source: review.posting_id, revision: expected, date: review.posting_date, amount, principal: principal_delta, interest: interest_delta, reversal: None, evidence: serde_json::json!({"classification":"HISTORICAL_VALUE_DATE_RECONCILIATION","reason":reason,"reconciliationReference":reference,"preview":review}) }).await?);
    }
    store::execute(
        tx,
        "UPDATE loan_account SET accrued_through=$3,rounding_carry=$4 WHERE tenant_id=$1 AND id=$2",
        vec![
            actor.tenant_id.into(),
            loan.into(),
            review.posting_date.into(),
            review.carry.into(),
        ],
    )
    .await?;
    let version = posting::bump_account(tx, actor.tenant_id, loan).await?;
    let mut result = lifecycle::result(
        review.posting_id,
        Some(loan),
        Some(account.employee_id),
        version,
        "RECONCILED",
    );
    result.posting_ids = ids;
    Ok(result)
}
