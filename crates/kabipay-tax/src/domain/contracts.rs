use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum TaxRegime {
    Old,
    New,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WithholdingMethod {
    AnnualProjection,
    PercentageOverride,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum EvidenceKind {
    ImportedActual,
    FinalizedPayroll,
    HistoricalEstimate,
    FutureProjection,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum CoverageStatus {
    Complete,
    Incomplete,
}

pub fn validate_amount(value: Decimal) -> KabiPayResult<()> {
    // PostgreSQL NUMERIC(15,4) pads stored rupees with zeroes. Validate financial
    // precision, not the representation's padding; never round away fractional paise.
    if value < Decimal::ZERO
        || value > Decimal::from(1_000_000_000_000i64)
        || value.normalize().scale() > 2
    {
        return Err(KabiPayError::Validation("amount must be nonnegative with at most two decimal places and within the supported limit".into()));
    }
    Ok(())
}
pub fn validate_year(year: i32) -> KabiPayResult<()> {
    if !(2000..=2199).contains(&year) {
        return Err(KabiPayError::Validation("unsupported fiscal year".into()));
    }
    Ok(())
}
pub fn validate_reason(reason: Option<&str>) -> KabiPayResult<()> {
    if reason.is_none_or(|r| r.trim().is_empty() || r.len() > 2000) {
        return Err(KabiPayError::Validation(
            "a reason of 1 to 2000 characters is required".into(),
        ));
    }
    Ok(())
}
