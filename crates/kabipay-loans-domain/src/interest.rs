use crate::{
    money::{add, div, mul, sub},
    Currency, InterestConvention, InterestMethod, LoanDomainError, PartialPeriodTreatment,
    RoundingRule,
};
use chrono::{Datelike, Months, NaiveDate};
use rust_decimal::Decimal;
#[derive(Clone, Debug)]
pub struct AccrualInput {
    pub principal: Decimal,
    pub method: InterestMethod,
    pub start: NaiveDate,
    pub end_exclusive: NaiveDate,
    pub carry: Decimal,
    pub currency: Currency,
    pub rounding: RoundingRule,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AccrualResult {
    pub posted_interest: Decimal,
    pub carry: Decimal,
}
pub fn accrue_interest(input: &AccrualInput) -> Result<AccrualResult, LoanDomainError> {
    input.currency.validate_amount(input.principal)?;
    input.method.validate(&input.currency)?;
    let unit = Decimal::new(1, input.currency.minor_units);
    if input.start > input.end_exclusive || input.carry.abs() >= unit {
        return Err(LoanDomainError::InvalidTerms);
    }
    let raw = match &input.method {
        InterestMethod::InterestFree => {
            if !input.carry.is_zero() {
                return Err(LoanDomainError::InvalidTerms);
            }
            Decimal::ZERO
        }
        InterestMethod::OneTimePercentage { rate } => {
            div(mul(input.principal, *rate)?, Decimal::from(100))?
        }
        InterestMethod::OneTimeFixed {
            amount,
            approved_principal,
        } => {
            if input.principal > *approved_principal {
                return Err(LoanDomainError::InvalidTerms);
            }
            div(mul(*amount, input.principal)?, *approved_principal)?
        }
        InterestMethod::ReducingBalance {
            annual_rate,
            convention,
        } => {
            let annual = div(mul(input.principal, *annual_rate)?, Decimal::from(100))?;
            match convention {
                InterestConvention::Act365Fixed => div(
                    mul(
                        annual,
                        Decimal::from((input.end_exclusive - input.start).num_days()),
                    )?,
                    Decimal::from(365),
                )?,
                InterestConvention::Monthly { partial_period } => {
                    monthly(annual, input.start, input.end_exclusive, *partial_period)?
                }
            }
        }
    };
    let exact = add(raw, input.carry)?;
    let posted = input.rounding.round(exact, input.currency.minor_units);
    if posted < Decimal::ZERO {
        return Err(LoanDomainError::InvalidTerms);
    }
    Ok(AccrualResult {
        posted_interest: posted,
        carry: sub(exact, posted)?,
    })
}
fn monthly(
    annual: Decimal,
    start: NaiveDate,
    end: NaiveDate,
    treatment: PartialPeriodTreatment,
) -> Result<Decimal, LoanDomainError> {
    let monthly = div(annual, Decimal::from(12))?;
    let mut cursor = start;
    let mut total = Decimal::ZERO;
    while cursor < end {
        let month = cursor.with_day(1).ok_or(LoanDomainError::InvalidTerms)?;
        let next = month
            .checked_add_months(Months::new(1))
            .ok_or(LoanDomainError::InvalidTerms)?;
        let stop = end.min(next);
        let full = cursor == month && stop == next;
        if !full && matches!(treatment, PartialPeriodTreatment::Reject) {
            return Err(LoanDomainError::PartialPeriod);
        }
        let ratio = if full {
            Decimal::ONE
        } else {
            div(
                Decimal::from((stop - cursor).num_days()),
                Decimal::from((next - month).num_days()),
            )?
        };
        total = add(total, mul(monthly, ratio)?)?;
        cursor = stop;
    }
    Ok(total)
}
