use crate::{
    posting::{self, Posting},
    repository as store, LoanActorScope, LoanModuleError, LoanResult,
};
use chrono::NaiveDate;
use kabipay_loans_domain::{accrue_calendar_interest, AccrualInput, LoanTerms};
use rust_decimal::Decimal;
use sea_orm::DatabaseTransaction;
use uuid::Uuid;

pub(crate) struct EarnedInterest {
    pub amount: Decimal,
    pub carry: Decimal,
    pub evidence: serde_json::Value,
}
// Keep the locked storage context and the complete accrual interval explicit at callers.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn calculate(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    loan: Uuid,
    terms: &LoanTerms,
    principal: Decimal,
    start: Option<NaiveDate>,
    end: NaiveDate,
    carry: Decimal,
) -> LoanResult<EarnedInterest> {
    if start.is_some_and(|date| date > end) {
        return Err(LoanModuleError::HistoricalReconciliationRequired);
    }
    let pauses = crate::arrangement::overrides(tx, tenant, loan)
        .await?
        .into_iter()
        .filter(|value| value.pause_interest)
        .map(|value| value.period)
        .collect::<Vec<_>>();
    let earned = accrue_calendar_interest(
        &AccrualInput {
            principal,
            method: terms.interest.clone(),
            start: start.unwrap_or(end),
            end_exclusive: end,
            carry,
            currency: terms.currency.clone(),
            rounding: terms.rounding,
        },
        &pauses,
    )?;
    Ok(EarnedInterest {
        amount: earned.posted_interest,
        carry: earned.carry,
        evidence: serde_json::json!({"calculatorVersion":terms.calculator_version,"principal":principal,"carryBefore":carry,"carryAfter":earned.carry,"intervals":earned.intervals}),
    })
}
// Posting binds the interval to both its immutable terms and originating source revision.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn accrue(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    loan: Uuid,
    terms_id: Uuid,
    terms: &LoanTerms,
    date: NaiveDate,
    source: Uuid,
    revision: i64,
) -> LoanResult<Vec<Uuid>> {
    let account = store::account(tx, actor.tenant_id, loan).await?;
    let (principal, _) = store::balances(tx, actor.tenant_id, loan).await?;
    let earned = calculate(
        tx,
        actor.tenant_id,
        loan,
        terms,
        principal,
        account.accrued_through,
        date,
        account.rounding_carry,
    )
    .await?;
    let mut ids = vec![];
    if earned.amount > Decimal::ZERO {
        ids.push(
            posting::post(
                tx,
                actor,
                Posting {
                    loan,
                    terms: terms_id,
                    kind: "INTEREST",
                    source_kind: "ACCRUAL",
                    source,
                    revision,
                    date,
                    amount: earned.amount,
                    principal: Decimal::ZERO,
                    interest: earned.amount,
                    reversal: None,
                    evidence: earned.evidence,
                },
            )
            .await?,
        );
    }
    store::execute(
        tx,
        "UPDATE loan_account SET accrued_through=$3,rounding_carry=$4 WHERE tenant_id=$1 AND id=$2",
        vec![
            actor.tenant_id.into(),
            loan.into(),
            date.into(),
            earned.carry.into(),
        ],
    )
    .await?;
    Ok(ids)
}
