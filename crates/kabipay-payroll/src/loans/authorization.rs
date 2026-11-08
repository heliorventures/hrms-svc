use async_graphql::{Context, ErrorExtensions, Result};
use kabipay_common::{
    context::ClientClaims,
    subgraph::{require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_loans::{LoanActorScope, LoanModuleError, LoanPermission};
use sea_orm::{DatabaseTransaction, TransactionTrait};
pub(super) fn financial_error(error: LoanModuleError) -> async_graphql::Error {
    use LoanModuleError::*;
    let (code, message) = match error {
        Authority(error) => return error.into_graphql(),
        Forbidden => (
            "FORBIDDEN",
            "This loan operation is outside your authority.",
        ),
        NotFound => ("LOAN_NOT_FOUND", "The loan record is unavailable."),
        VersionConflict => (
            "LOAN_REVIEW_STALE",
            "Loan information changed. Refresh and review it again.",
        ),
        ExitReviewPending => (
            "LOAN_EXIT_REVIEW_PENDING",
            "HR exit approval is pending confirmation. Reconcile it before changing loan records.",
        ),
        ReconciliationBlocked => ("LOAN_RECONCILIATION_BLOCKED", "Loan recovery is held because financial reconciliation found an exception. HR must review the reconciliation report."),
        IdempotencyConflict => (
            "LOAN_COMMAND_CONFLICT",
            "This command key has already been used for different input.",
        ),
        SourceAlreadyPosted => (
            "LOAN_SOURCE_ALREADY_POSTED",
            "This financial source has already been posted.",
        ),
        PolicyUnavailable => (
            "LOAN_POLICY_UNAVAILABLE",
            "An applicable configured loan policy is required.",
        ),
        ExposureExceeded => (
            "LOAN_EXPOSURE_LIMIT",
            "The configured loan exposure limit would be exceeded.",
        ),
        FundingCeilingExceeded => (
            "LOAN_FUNDING_LIMIT",
            "Disbursement exceeds the approved funding balance.",
        ),
        ExcessPayment => (
            "LOAN_EXCESS_PAYMENT",
            "Payment exceeds the balance and policy does not permit held credit.",
        ),
        HistoricalReconciliationRequired => (
            "LOAN_RECONCILIATION_REQUIRED",
            "This change requires review of the existing financial history.",
        ),
        PriorityConflict => (
            "LOAN_PRIORITY_CONFLICT",
            "Review inconsistent loan recovery priorities.",
        ),
        RecoveryCapacity => (
            "LOAN_RECOVERY_CAPACITY",
            "Salary cannot satisfy the configured recovery protection.",
        ),
        RepaymentReviewRequired => (
            "LOAN_REPAYMENT_REVIEW_REQUIRED",
            "HR must review the remaining loan balance and configure repayment dates within the approved policy before payroll can proceed.",
        ),
        Domain(_) | InvalidCommand => (
            "LOAN_INVALID_INPUT",
            "Loan input is incomplete, unsupported or outside the configured policy.",
        ),
        Database(_) | Serialization(_) => {
            ("LOAN_INTERNAL_ERROR", "Loan processing could not complete.")
        }
    };
    async_graphql::Error::new(message).extend_with(|_, extensions| extensions.set("code", code))
}
pub(super) async fn transaction(
    ctx: &Context<'_>,
    permission: LoanPermission,
) -> Result<(DatabaseTransaction, LoanActorScope)> {
    let tenant = require_tenant_id(ctx)?;
    let claims = ctx
        .data::<ClientClaims>()
        .map_err(|_| KabiPayError::Unauthorised.into_graphql())?;
    let actor = LoanActorScope::from_verified_claims(claims, tenant, permission)
        .map_err(financial_error)?;
    let db = tenant_db(ctx, tenant).await?;
    let tx = db
        .begin()
        .await
        .map_err(|error| KabiPayError::from(error).into_graphql())?;
    Ok((tx, actor))
}
