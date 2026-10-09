//! Loans owns storage. This hosting layer only translates authenticated GraphQL contracts.
mod authorization;
mod mutation;
mod query;
mod types;
pub use mutation::LoanMutation;
pub use query::LoanQuery;
