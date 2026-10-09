use crate::{
    accrue_interest, allocate_repayment,
    money::{add, sub},
    AccrualInput, AllocationInput, InterestMethod, LoanDomainError, LoanTerms,
};
use chrono::{Datelike, Months, NaiveDate};
use rust_decimal::Decimal;
#[derive(Clone, Debug)]
pub struct PeriodOverride {
    pub period: NaiveDate,
    pub amount: Option<Decimal>,
    pub pause_interest: bool,
}
#[derive(Clone, Debug)]
pub struct ScheduleInput {
    pub terms: LoanTerms,
    pub principal: Decimal,
    pub due_interest: Decimal,
    pub carry: Decimal,
    pub accrual_start: NaiveDate,
    pub first_due_date: NaiveDate,
    pub overrides: Vec<PeriodOverride>,
}
#[derive(Clone, Debug, PartialEq)]
pub struct ScheduledRecovery {
    pub due_date: NaiveDate,
    pub principal: Decimal,
    pub interest: Decimal,
    pub total: Decimal,
    pub principal_after: Decimal,
    pub interest_after: Decimal,
}
#[derive(Clone, Debug)]
pub struct LoanSchedule {
    pub items: Vec<ScheduledRecovery>,
    pub residual_principal: Decimal,
    pub residual_interest: Decimal,
    pub carry: Decimal,
}
pub fn project_schedule(input: &ScheduleInput) -> Result<LoanSchedule, LoanDomainError> {
    input.terms.validate()?;
    input.terms.currency.validate_amount(input.principal)?;
    input.terms.currency.validate_amount(input.due_interest)?;
    if input.first_due_date < input.accrual_start
        || input.principal > input.terms.approved_principal
        || input.carry.abs() >= Decimal::new(1, input.terms.currency.minor_units)
    {
        return Err(LoanDomainError::InvalidTerms);
    }
    for (index, item) in input.overrides.iter().enumerate() {
        if item.period.day() != 1
            || input.overrides[..index]
                .iter()
                .any(|o| o.period == item.period)
        {
            return Err(LoanDomainError::InvalidTerms);
        }
        if let Some(amount) = item.amount {
            input.terms.currency.validate_amount(amount)?;
            if amount.is_zero() {
                return Err(LoanDomainError::InvalidTerms);
            }
        }
    }
    let (mut principal, mut interest, mut carry) =
        (input.principal, input.due_interest, input.carry);
    let mut start = input.accrual_start;
    let mut items = Vec::new();
    for n in 0..input.terms.max_instalments {
        if principal.is_zero() && interest.is_zero() {
            break;
        }
        let due_date = input
            .first_due_date
            .checked_add_months(Months::new(n))
            .ok_or(LoanDomainError::InvalidTerms)?;
        let period = due_date.with_day(1).ok_or(LoanDomainError::InvalidTerms)?;
        let override_ = input.overrides.iter().find(|o| o.period == period);
        let monthly = override_.map_or(Some(input.terms.monthly_amount), |o| o.amount);
        if !override_.is_some_and(|o| o.pause_interest)
            && matches!(input.terms.interest, InterestMethod::ReducingBalance { .. })
        {
            let method = input.terms.interest.clone();
            // One-time charges were recognized at funding; never assess them again on due dates.
            let accrual = accrue_interest(&AccrualInput {
                principal,
                method,
                start,
                end_exclusive: due_date,
                carry,
                currency: input.terms.currency.clone(),
                rounding: input.terms.rounding,
            })?;
            interest = add(interest, accrual.posted_interest)?;
            carry = accrual.carry;
        }
        let allocation = allocate_repayment(&AllocationInput {
            payment: monthly.unwrap_or(Decimal::ZERO),
            principal,
            interest,
            order: input.terms.allocation,
            currency: input.terms.currency.clone(),
        })?;
        principal = sub(principal, allocation.principal)?;
        interest = sub(interest, allocation.interest)?;
        items.push(ScheduledRecovery {
            due_date,
            principal: allocation.principal,
            interest: allocation.interest,
            total: add(allocation.principal, allocation.interest)?,
            principal_after: principal,
            interest_after: interest,
        });
        start = due_date;
    }
    if (!principal.is_zero() || !interest.is_zero()) && !input.terms.allow_residual {
        return Err(LoanDomainError::NonAmortizing);
    }
    Ok(LoanSchedule {
        items,
        residual_principal: principal,
        residual_interest: interest,
        carry,
    })
}
