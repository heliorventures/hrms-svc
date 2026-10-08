use super::validate_amount;
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct WithholdingAllocation {
    pub monthly: Decimal,
    pub remaining: Option<Decimal>,
    pub requires_acknowledgement: bool,
    pub excess_withholding: bool,
}
pub fn allocate(
    annual: Decimal,
    prior: Option<Decimal>,
    employment_months: u32,
    remaining_months: u32,
) -> KabiPayResult<WithholdingAllocation> {
    validate_amount(annual)?;
    if let Some(v) = prior {
        validate_amount(v)?;
    }
    if employment_months == 0
        || employment_months > 12
        || remaining_months == 0
        || remaining_months > employment_months
    {
        return Err(KabiPayError::Validation(
            "no eligible unfinalized payroll months in the tax year".into(),
        ));
    }
    let remaining = prior.map(|v| (annual - v).max(Decimal::ZERO));
    let monthly = (remaining.unwrap_or(annual)
        / Decimal::from(if prior.is_some() {
            remaining_months
        } else {
            employment_months
        }))
    .round_dp_with_strategy(2, RoundingStrategy::MidpointAwayFromZero);
    Ok(WithholdingAllocation {
        monthly,
        remaining,
        requires_acknowledgement: prior.is_none(),
        excess_withholding: prior.is_some_and(|v| v > annual),
    })
}
