//! Backward-compatible HR approval proves that the employee has no Loan exposure.
use kabipay_common::{
    context::ClientClaims, finance_contract::ExitSubject,
    tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult,
};
use kabipay_loans::{ExitFinancialDecision, LoanActorScope, LoanPermission, SaveExitReview};
use sea_orm::{DatabaseConnection, TransactionTrait};
use uuid::Uuid;

pub(super) async fn prepare(
    db: &DatabaseConnection,
    ops: &DatabaseConnection,
    tenant: Uuid,
    claims: &ClientClaims,
    subject: &ExitSubject,
) -> KabiPayResult<serde_json::Value> {
    let actor = LoanActorScope::from_verified_claims(claims, tenant, LoanPermission::FnfRecovery)
        .map_err(super::coordination::financial_error)?;
    let date = TenantBusinessClock::load(ops, tenant)
        .await?
        .now_date()
        .max(subject.last_working_date);
    let tx = db.begin().await?;
    let exposure = kabipay_loans::preview_exit_exposure(&tx, &actor, subject.employee_id, date)
        .await
        .map_err(super::coordination::financial_error)?;
    if !exposure.loans.is_empty() {
        return Err(KabiPayError::BusinessRule { code: "LOAN_EXIT_REVIEW_REQUIRED", message: "Review the employee's loans and repayment arrangement before approving the separation".into() });
    }
    let review = kabipay_loans::save_exit_review(
        &tx,
        &actor,
        &SaveExitReview {
            separation_id: subject.separation_id,
            employee_id: subject.employee_id,
            separation_fingerprint: subject.fingerprint.clone(),
            last_working_date: subject.last_working_date,
            settlement_date: date,
            exposure_fingerprint: exposure.fingerprint,
            decision: ExitFinancialDecision {
                reason: "Verified no employee Loan exposure at HR approval review".into(),
                loans: vec![],
            },
        },
    )
    .await
    .map_err(super::coordination::financial_error)?;
    tx.commit().await?;
    Ok(serde_json::json!({"reviewId":review.id,"separationId":review.separation_id}))
}
