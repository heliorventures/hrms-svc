//! Internal command authentication. Verification never replaces live domain authorization.
mod config;
mod delegation;
mod service;
pub use config::*;
pub use delegation::*;
pub use service::*;
