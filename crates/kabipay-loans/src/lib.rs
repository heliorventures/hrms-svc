//! Loan module: public contracts with private storage and caller-owned transactions.
mod access;
mod accrual;
mod arrangement;
mod audit;
mod command;
mod contracts;
mod correction;
mod exit_authorization;
mod exit_review;
mod exit_servicing;
mod inputs;
mod historical_payment;
mod replay;
mod lifecycle;
mod payments;
mod payroll_state;
mod permissions;
mod posting;
mod projection;
mod read;
mod reconciliation;
mod recovery;
mod repository;
mod service;
mod workflow;
pub use command::command_hash;
pub use audit::*;
pub use contracts::*;
pub use exit_authorization::*;
pub use exit_review::*;
pub use exit_servicing::confirm_fnf_arrangements;
pub use inputs::*;
pub use historical_payment::{preview_historical_payment, HistoricalPaymentPreview};
pub use payroll_state::{payroll_loan_state, PayrollLoanState};
pub use permissions::*;
pub use read::*;
pub use projection::*;
pub use reconciliation::{preview_loan_reversal, LoanCorrectionPreview};
pub use recovery::*;
pub use service::{execute_command, execute_with_funding_context, funding_employee, recorded_command};
#[derive(Debug, thiserror::Error)]
pub enum LoanModuleError {
    #[error("loan terms are invalid: {0}")]
    Domain(#[from] kabipay_loans_domain::LoanDomainError),
    #[error("loan authorization failed: {0}")]
    Authority(#[from] kabipay_common::KabiPayError),
    #[error("loan storage failed")]
    Database(sea_orm::DbErr),
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
    #[error("loan inputs are held for exit approval; reconcile the HR operation before retrying")]
    ExitReviewPending,
    #[error("loan recovery is held for financial reconciliation")]
    ReconciliationBlocked,
}
pub type LoanResult<T> = Result<T, LoanModuleError>;

impl From<sea_orm::DbErr> for LoanModuleError {
    fn from(error: sea_orm::DbErr) -> Self {
        if error.to_string().contains(
            "loan_exit_approval_guard: reviewed loan inputs await authoritative HR confirmation",
        ) {
            Self::ExitReviewPending
        } else {
            Self::Database(error)
        }
    }
}
