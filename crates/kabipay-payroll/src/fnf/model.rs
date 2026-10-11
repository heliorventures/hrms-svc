use super::calculation::{PayoutPool, ReviewedPayoutSource};
use chrono::NaiveDate;
use kabipay_loans::{ExitFinancialDecision, LoanExitExposure, RecoveryQuote};
use kabipay_loans_domain::Currency;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FinancialInput {
    pub separation_id: Uuid,
    pub value_date: NaiveDate,
    pub currency: Currency,
    pub exit_review_id: Option<Uuid>,
    pub financial_approval_reference: String,
    pub sources: Vec<ReviewedPayoutSource>,
    pub loan_decision: ExitFinancialDecision,
    pub legacy_review: Option<LegacyReview>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LegacyReview {
    pub original_figures_fingerprint: String,
    pub reason: String,
    pub classify_as_other_recoveries: bool,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinancialSnapshot {
    pub input: FinancialInput,
    pub separation_fingerprint: String,
    pub loan_state_fingerprint: String,
    pub exposure: LoanExitExposure,
    pub recovery_quote: Option<RecoveryQuote>,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub available_payout: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub eligible_payout: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub other_recoveries: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub loan_recovered: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub employee_payout: Decimal,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub residual_loan_receivable: Decimal,
}

impl FinancialSnapshot {
    pub(super) fn from_pool(
        input: FinancialInput,
        separation_fingerprint: String,
        loan_state_fingerprint: String,
        exposure: LoanExitExposure,
        recovery_quote: Option<RecoveryQuote>,
        pool: PayoutPool,
    ) -> kabipay_common::KabiPayResult<Self> {
        let recovered = recovery_quote
            .as_ref()
            .map_or(Decimal::ZERO, RecoveryQuote::total);
        let debt = exposure
            .loans
            .iter()
            .try_fold(Decimal::ZERO, |sum, line| {
                sum.checked_add(line.forecast_payoff)
            })
            .ok_or_else(|| {
                kabipay_common::KabiPayError::Validation(
                    "FNF debt exceeds the supported amount".into(),
                )
            })?;
        let employee_payout = pool
            .available
            .checked_sub(recovered)
            .filter(|v| *v >= Decimal::ZERO)
            .ok_or_else(|| {
                kabipay_common::KabiPayError::Validation(
                    "Loan recovery exceeds available payout".into(),
                )
            })?;
        let residual = debt
            .checked_sub(recovered)
            .filter(|v| *v >= Decimal::ZERO)
            .ok_or_else(|| {
                kabipay_common::KabiPayError::Validation(
                    "Loan recovery exceeds the outstanding debt".into(),
                )
            })?;
        Ok(Self {
            input,
            separation_fingerprint,
            loan_state_fingerprint,
            exposure,
            recovery_quote,
            available_payout: pool.available,
            eligible_payout: pool.eligible_for_loan,
            other_recoveries: pool.other_recoveries,
            loan_recovered: recovered,
            employee_payout,
            residual_loan_receivable: residual,
        })
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinancialReview {
    pub fnf_id: Uuid,
    pub revision: i64,
    pub fingerprint: String,
    pub snapshot: FinancialSnapshot,
    pub finalized: bool,
}
