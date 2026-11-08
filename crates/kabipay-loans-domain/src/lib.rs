//! Versioned, currency-safe loan calculations. No storage or transport dependencies.
mod allocation;
pub mod decimal_string;
mod interest;
mod money;
mod policy;
mod schedule;
mod state;
mod terms;
pub use allocation::*;
pub use interest::*;
pub use money::*;
pub use policy::*;
pub use schedule::*;
pub use state::*;
pub use terms::*;
#[derive(Debug, thiserror::Error, PartialEq)]
pub enum LoanDomainError {
    #[error("loan terms are incomplete or invalid")]
    InvalidTerms,
    #[error("loan monetary arithmetic overflow")]
    ArithmeticOverflow,
    #[error("monthly accrual requires an explicit partial-period convention")]
    PartialPeriod,
    #[error("repayment arrangement does not amortize within the approved tenure")]
    NonAmortizing,
}
