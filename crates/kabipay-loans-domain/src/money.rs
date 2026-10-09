use crate::LoanDomainError;
use rust_decimal::{Decimal, RoundingStrategy};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct Currency {
    pub code: String,
    pub minor_units: u32,
}
impl Currency {
    pub fn validate(&self) -> Result<(), LoanDomainError> {
        if self.code.len() != 3
            || !self.code.bytes().all(|b| b.is_ascii_uppercase())
            || self.minor_units > 4
        {
            return Err(LoanDomainError::InvalidTerms);
        }
        Ok(())
    }
    pub fn validate_amount(&self, amount: Decimal) -> Result<(), LoanDomainError> {
        self.validate()?;
        if amount < Decimal::ZERO
            || amount >= Decimal::from(10_000_000_000_000_000u64)
            || amount.round_dp(self.minor_units) != amount
        {
            return Err(LoanDomainError::InvalidTerms);
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RoundingRule {
    HalfUp,
    HalfEven,
}
impl RoundingRule {
    pub fn round(self, amount: Decimal, units: u32) -> Decimal {
        amount.round_dp_with_strategy(
            units,
            match self {
                Self::HalfUp => RoundingStrategy::MidpointAwayFromZero,
                Self::HalfEven => RoundingStrategy::MidpointNearestEven,
            },
        )
    }
}
pub(crate) fn add(a: Decimal, b: Decimal) -> Result<Decimal, LoanDomainError> {
    a.checked_add(b).ok_or(LoanDomainError::ArithmeticOverflow)
}
pub(crate) fn sub(a: Decimal, b: Decimal) -> Result<Decimal, LoanDomainError> {
    a.checked_sub(b).ok_or(LoanDomainError::ArithmeticOverflow)
}
pub(crate) fn mul(a: Decimal, b: Decimal) -> Result<Decimal, LoanDomainError> {
    a.checked_mul(b).ok_or(LoanDomainError::ArithmeticOverflow)
}
pub(crate) fn div(a: Decimal, b: Decimal) -> Result<Decimal, LoanDomainError> {
    a.checked_div(b).ok_or(LoanDomainError::ArithmeticOverflow)
}
