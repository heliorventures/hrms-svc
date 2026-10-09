use crate::{money::sub, AllocationOrder, Currency, LoanDomainError};
use rust_decimal::Decimal;
#[derive(Clone, Debug)]
pub struct AllocationInput {
    pub payment: Decimal,
    pub principal: Decimal,
    pub interest: Decimal,
    pub order: AllocationOrder,
    pub currency: Currency,
}
#[derive(Clone, Debug, PartialEq)]
pub struct AllocationResult {
    pub principal: Decimal,
    pub interest: Decimal,
    pub excess_credit: Decimal,
}
pub fn allocate_repayment(input: &AllocationInput) -> Result<AllocationResult, LoanDomainError> {
    for amount in [input.payment, input.principal, input.interest] {
        input.currency.validate_amount(amount)?;
    }
    let (principal, interest) = match input.order {
        AllocationOrder::InterestFirst => {
            let interest = input.interest.min(input.payment);
            (input.principal.min(sub(input.payment, interest)?), interest)
        }
        AllocationOrder::PrincipalFirst => {
            let principal = input.principal.min(input.payment);
            (
                principal,
                input.interest.min(sub(input.payment, principal)?),
            )
        }
    };
    Ok(AllocationResult {
        principal,
        interest,
        excess_credit: sub(sub(input.payment, principal)?, interest)?,
    })
}
