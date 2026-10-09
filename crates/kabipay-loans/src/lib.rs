//! Loan module: public contracts with private storage and caller-owned transactions.
mod access;
mod accrual;
mod arrangement;
mod command;
mod contracts;
mod correction;
mod inputs;
mod lifecycle;
mod payments;
mod permissions;
mod payroll_state;
mod posting;
mod read;
mod reconciliation;
mod recovery;
mod repository;
mod service;
mod workflow;
pub use command::command_hash;
pub use contracts::*;
pub use inputs::*;
pub use permissions::*;
pub use payroll_state::{payroll_loan_state, PayrollLoanState};
pub use read::*;
pub use reconciliation::{preview_loan_reversal, LoanCorrectionPreview};
pub use recovery::*;
pub use service::execute_command;
#[derive(Debug, thiserror::Error)]
pub enum LoanModuleError {
    #[error("loan terms are invalid: {0}")]
    Domain(#[from] kabipay_loans_domain::LoanDomainError),
    #[error("loan authorization failed: {0}")]
    Authority(#[from] kabipay_common::KabiPayError),
    #[error("loan storage failed")]
    Database(#[from] sea_orm::DbErr),
    #[error("loan serialization failed")]
    Serialization(#[from] serde_json::Error),
    #[error("loan review is stale")]
    VersionConflict,
    #[error("loan record was not found")]
    NotFound,
    #[error("loan policy is unavailable")]
    PolicyUnavailable,
    #[error("loan command key was reused with different input")]
    IdempotencyConflict,
    #[error("loan source is already posted")]
    SourceAlreadyPosted,
    #[error("loan operation is outside the actor's authority")]
    Forbidden,
    #[error("loan command is invalid")]
    InvalidCommand,
    #[error("loan exposure exceeds the configured policy")]
    ExposureExceeded,
    #[error("loan funding exceeds the approved principal")]
    FundingCeilingExceeded,
    #[error("loan payment exceeds the payable balance")]
    ExcessPayment,
    #[error("loan history requires reviewed reconciliation")]
    HistoricalReconciliationRequired,
    #[error("loan recovery priorities require a consistent reviewed selection")]
    PriorityConflict,
    #[error("salary cannot satisfy the configured loan recovery protection")]
    RecoveryCapacity,
}
pub type LoanResult<T> = Result<T, LoanModuleError>;
