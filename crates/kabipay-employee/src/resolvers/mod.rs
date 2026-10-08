//! GraphQL resolvers for kabipay-employee.
//!
//! Resolvers are the only place that imports `async_graphql`. Business logic lives
//! in `crate::services::*`. This keeps services unit-testable without a GraphQL ctx.

pub mod mutation;
pub mod query;
pub mod scope;
pub mod company_location_types;
pub mod types;
pub mod prejoining;
pub mod prejoining_options;
#[cfg(test)]
mod guidance_tests;

pub use mutation::MutationRoot;
pub use query::QueryRoot;
