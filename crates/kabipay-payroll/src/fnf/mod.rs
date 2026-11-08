//! Sole financial writer for final settlements; Employee owns employment and clearance.
pub(crate) mod coordination;
pub mod http;
pub mod exit_resolvers;
mod empty_review;
pub mod worker;
pub mod calculation;
pub mod model;
mod review;
mod decision_authority;
pub mod payroll_context;
pub mod draft;
pub mod finalize;
mod integrity;
pub mod legacy;
mod types;
pub mod resolvers;
pub(crate) mod source;
