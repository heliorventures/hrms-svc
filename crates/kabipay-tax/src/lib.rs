//! Shared tax domain and tenant-scoped services; no dependency on payroll.
pub mod domain;
pub mod resolvers;
pub mod services;
pub mod email_alerts;

mod email_emit;
