//! Preserve legacy IDs and generic other recoveries; never invent Loan allocations.
use super::{coordination::financial_error, draft, source};
use kabipay_common::{
    context::ClientClaims, finance_contract::ExitSubject, KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::fnf_settlement;
use rust_decimal::Decimal;
use sea_orm::{ActiveModelTrait, DatabaseConnection, Set, TransactionTrait};
use std::str::FromStr;
use uuid::Uuid;

fn amount(value: &Option<String>) -> KabiPayResult<Option<Decimal>> {
    match value.as_deref().map(str::trim).filter(|v| !v.is_empty()) {
        None => Ok(None),
        Some(value) => {
            let amount = Decimal::from_str(value)
                .map_err(|_| KabiPayError::Validation("Invalid FNF decimal amount".into()))?;
            if amount < Decimal::ZERO
                || amount >= Decimal::from(10_000_000_000_000_000u64)
                || amount.round_dp(4) != amount
            {
                return Err(KabiPayError::Validation(
                    "FNF amounts must be nonnegative supported decimal values".into(),
                ));
            }
            Ok(Some(amount))
        }
    }
}

pub async fn save(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    subject: &ExitSubject,
    today: chrono::NaiveDate,
    leave: &Option<String>,
    gratuity: &Option<String>,
    bonus: &Option<String>,
    other: &Option<String>,
) -> KabiPayResult<fnf_settlement::Model> {
    if subject.status != "APPROVED" {
        return Err(KabiPayError::Validation(
            "HR must approve separation before FNF preparation".into(),
        ));
    }
    let actor = super::review::actor(claims, tenant)?;
    let tx = db.begin().await?;
    crate::services::loan_recovery::lock(&tx, tenant).await?;
    let row = source::ensure_draft(&tx, tenant, subject.separation_id).await?;
    if row.status != "DRAFT" || draft::load(&tx, tenant, row.id, true).await?.is_some() {
        return Err(KabiPayError::Conflict(
            "Edit the reviewed FNF sources; issued records cannot change".into(),
        ));
    }
    let exposure = kabipay_loans::preview_exit_exposure(&tx, &actor, subject.employee_id, today)
        .await
        .map_err(financial_error)?;
    if !exposure.loans.is_empty() {
        return Err(KabiPayError::BusinessRule { code:"LOAN_FNF_REVIEW_REQUIRED",message:"Review itemized payout sources, Loan recovery and residual debt for this settlement".into() });
    }
    let leave = amount(leave)?;
    let gratuity = amount(gratuity)?;
    let bonus = amount(bonus)?;
    let other = amount(other)?;
    let total = [leave, gratuity, bonus]
        .into_iter()
        .try_fold(Decimal::ZERO, |sum, value| {
            sum.checked_add(value.unwrap_or(Decimal::ZERO))
        })
        .ok_or_else(|| KabiPayError::Validation("FNF total exceeds the supported amount".into()))?;
    let net = total
        .checked_sub(other.unwrap_or(Decimal::ZERO))
        .filter(|value| *value >= Decimal::ZERO)
        .ok_or_else(|| {
            KabiPayError::Validation("Other recoveries exceed the available FNF amounts".into())
        })?;
    let mut active: fnf_settlement::ActiveModel = row.into();
    active.leave_encashment = Set(leave);
    active.gratuity_amount = Set(gratuity);
    active.bonus_payable = Set(bonus);
    active.recovery_amount = Set(other);
    active.net_payable = Set(Some(net));
    active.updated_at = Set(chrono::Utc::now());
    let row = active.update(&tx).await?;
    tx.commit().await?;
    Ok(row)
}
