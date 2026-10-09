use crate::{
    posting::{self, Posting},
    repository as store, LoanActorScope, LoanModuleError, LoanResult,
};
use chrono::{Datelike, Months, NaiveDate};
use kabipay_loans_domain::{accrue_interest, AccrualInput, InterestMethod, LoanTerms};
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
    let mut interest = Decimal::ZERO;
    let mut carry_after = carry;
    let mut intervals = vec![];
    if let (InterestMethod::ReducingBalance { .. }, Some(mut cursor)) = (&terms.interest, start) {
        while cursor < end {
            let period = cursor.with_day(1).ok_or(LoanModuleError::InvalidCommand)?;
            let next = period
                .checked_add_months(Months::new(1))
                .ok_or(LoanModuleError::InvalidCommand)?
                .min(end);
            let pause=store::one(tx,"SELECT accrual_treatment FROM loan_period_override WHERE tenant_id=$1 AND loan_id=$2 AND period_start=$3",vec![tenant.into(),loan.into(),period.into()]).await?.map(|row|row.try_get::<String>("","accrual_treatment")).transpose()?.is_some_and(|value|value=="PAUSE");
            let earned = if pause {
                Decimal::ZERO
            } else {
                let result = accrue_interest(&AccrualInput {
                    principal,
                    method: terms.interest.clone(),
                    start: cursor,
                    end_exclusive: next,
                    carry: carry_after,
                    currency: terms.currency.clone(),
                    rounding: terms.rounding,
                })?;
                carry_after = result.carry;
                result.posted_interest
            };
            interest = interest
                .checked_add(earned)
                .ok_or(LoanModuleError::InvalidCommand)?;
            intervals.push(serde_json::json!({"start":cursor,"endExclusive":next,"paused":pause,"posted":earned}));
            cursor = next;
        }
    }
    Ok(EarnedInterest {
        amount: interest,
        carry: carry_after,
        evidence: serde_json::json!({"calculatorVersion":terms.calculator_version,"principal":principal,"carryBefore":carry,"carryAfter":carry_after,"intervals":intervals}),
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
