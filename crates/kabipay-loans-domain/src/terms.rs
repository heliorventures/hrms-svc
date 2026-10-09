use crate::{Currency, LoanDomainError, RoundingRule};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
pub const CALCULATOR_VERSION: &str = "loans-v1";
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum PartialPeriodTreatment {
    Reject,
    ProrateActualDays,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
#[serde(rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum InterestConvention {
    Act365Fixed,
    Monthly {
        partial_period: PartialPeriodTreatment,
    },
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all_fields = "camelCase", deny_unknown_fields)]
pub enum InterestMethod {
    InterestFree,
    OneTimePercentage {
        #[serde(with = "crate::decimal_string")]
        rate: Decimal,
    },
    OneTimeFixed {
        #[serde(with = "crate::decimal_string")]
        amount: Decimal,
        #[serde(with = "crate::decimal_string")]
        approved_principal: Decimal,
    },
    ReducingBalance {
        #[serde(with = "crate::decimal_string")]
        annual_rate: Decimal,
        convention: InterestConvention,
    },
}
impl InterestMethod {
    pub fn validate(&self, currency: &Currency) -> Result<(), LoanDomainError> {
        match self {
            Self::InterestFree => Ok(()),
            Self::OneTimePercentage { rate }
            | Self::ReducingBalance {
                annual_rate: rate, ..
            } => {
                if *rate < Decimal::ZERO || rate.scale() > 6 {
                    return Err(LoanDomainError::InvalidTerms);
                }
                Ok(())
            }
            Self::OneTimeFixed {
                amount,
                approved_principal,
            } => {
                currency.validate_amount(*amount)?;
                currency.validate_amount(*approved_principal)?;
                if *approved_principal <= Decimal::ZERO {
                    return Err(LoanDomainError::InvalidTerms);
                }
                Ok(())
            }
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum AllocationOrder {
    InterestFirst,
    PrincipalFirst,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryMode {
    Payroll,
    External,
    Mixed,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoanTerms {
    pub currency: Currency,
    #[serde(with = "crate::decimal_string")]
    pub approved_principal: Decimal,
    pub interest: InterestMethod,
    pub rounding: RoundingRule,
    pub allocation: AllocationOrder,
    pub recovery: RecoveryMode,
    #[serde(with = "crate::decimal_string")]
    pub monthly_amount: Decimal,
    pub max_instalments: u32,
    pub allow_residual: bool,
    pub calculator_version: String,
}
impl LoanTerms {
    pub fn validate(&self) -> Result<(), LoanDomainError> {
        self.currency.validate_amount(self.approved_principal)?;
        self.currency.validate_amount(self.monthly_amount)?;
        self.interest.validate(&self.currency)?;
        if self.approved_principal <= Decimal::ZERO
            || self.monthly_amount <= Decimal::ZERO
            || self.max_instalments == 0
            || self.max_instalments > 1200
            || self.calculator_version != CALCULATOR_VERSION
        {
            return Err(LoanDomainError::InvalidTerms);
        }
        if let InterestMethod::OneTimeFixed {
            approved_principal, ..
        } = &self.interest
        {
            if *approved_principal != self.approved_principal {
                return Err(LoanDomainError::InvalidTerms);
            }
        }
        Ok(())
    }
}
