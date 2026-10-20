//! Organization identities, explicit company authority and versioned business facts.
//! This crate has no database, HTTP, Kafka or tenant credential dependencies.

mod access;
mod event;
mod hierarchy;
mod identity;

pub use access::*;
pub use event::*;
pub use hierarchy::*;
pub use identity::*;

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum OrganizationError {
    #[error("identifier must not be nil")]
    InvalidIdentifier,
    #[error("version must be positive and fit a database bigint")]
    InvalidVersion,
    #[error("capability must contain a canonical resource and action")]
    InvalidCapability,
    #[error("effective period must have a strictly later end")]
    InvalidPeriod,
    #[error("organization authority is inactive or no longer current")]
    AuthorityUnavailable,
    #[error("operation is outside the explicit company grant")]
    OutsideGrant,
    #[error("primary reporting cannot refer to the same person or employment")]
    SelfReporting,
    #[error("employment has more than one active primary manager")]
    MultiplePrimaryManagers,
    #[error("primary reporting contains a cycle")]
    ReportingCycle,
    #[error("employment reference maps to inconsistent person identities")]
    InconsistentEmploymentIdentity,
    #[error("message source differs from the configured tenant binding")]
    WrongTenant,
    #[error("business event metadata is incomplete or unsupported")]
    InvalidEvent,
}
