//! HR's persisted operation is the authoritative outcome, never a caller's assertion.
use kabipay_common::{
    context::ClientClaims,
    finance_contract::{
        ExitOperationStatus, FinanceAction, FinanceAuthorizationState, HrOperationState,
    },
    KabiPayError, KabiPayResult,
};
use kabipay_loans::{ExitAuthorization, LoanActorScope, LoanPermission, RegisteredHrApproval};
use sea_orm::{DatabaseConnection, TransactionTrait};
use uuid::Uuid;

pub(crate) fn registered(operation: &ExitOperationStatus) -> RegisteredHrApproval {
    RegisteredHrApproval {
        operation_id: operation.id,
        separation_id: operation.separation_id,
        employee_id: operation.employee_id,
        review_id: operation.review_id,
        actor_id: operation.actor_id,
        separation_fingerprint: operation.separation_fingerprint.clone(),
        payload_hash: operation.payload_hash.clone(),
    }
}

pub(crate) fn validate_prepare(
    tenant: Uuid,
    claims: Option<&ClientClaims>,
    operation: &ExitOperationStatus,
) -> KabiPayResult<LoanActorScope> {
    let claims = claims.ok_or(KabiPayError::Unauthorised)?;
    if kabipay_common::client_data_scope::data_scope_from_claims(Some(claims), "onboarding:manage")?
        != kabipay_common::context::ScopeType::All
    {
        return Err(KabiPayError::Forbidden(
            "Exit approval requires ALL HR scope".into(),
        ));
    }
    if operation.actor_id != claims.sub
        || operation.state != HrOperationState::Registered
        || operation.id.is_nil()
        || operation.review_id.is_nil()
        || operation.employee_id.is_nil()
        || operation.separation_id.is_nil()
        || operation.authorization_id.is_some()
        || operation.payload_hash.len() != 64
        || operation.separation_fingerprint.len() != 64
    {
        return Err(KabiPayError::Conflict(
            "HR approval is not registered for this actor".into(),
        ));
    }
    LoanActorScope::from_verified_claims(claims, tenant, LoanPermission::FnfRecovery)
        .map_err(financial_error)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    fn inputs() -> (ClientClaims, ExitOperationStatus) {
        let claims = ClientClaims {
            sub: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(),
            exp: 2000,
            iat: 1000,
            email: String::new(),
            employee_id: None,
            must_change_password: false,
            roles: vec!["ADMIN".into()],
            permissions: vec!["onboarding:manage".into()],
            permission_scopes: HashMap::from([("onboarding:manage".into(), "ALL".into())]),
            resource_scopes: HashMap::new(),
        };
        let operation = ExitOperationStatus {
            id: Uuid::new_v4(),
            separation_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(),
            actor_id: claims.sub,
            review_id: Uuid::new_v4(),
            separation_fingerprint: "a".repeat(64),
            payload_hash: "b".repeat(64),
            authorization_id: None,
            state: HrOperationState::Registered,
        };
        (claims, operation)
    }

    #[test]
    fn prepare_requires_exact_hr_authority_actor_and_registered_operation() {
        let (mut claims, mut operation) = inputs();
        assert!(validate_prepare(claims.tenant_id, Some(&claims), &operation).is_ok());
        assert!(validate_prepare(Uuid::new_v4(), Some(&claims), &operation).is_err());
        assert!(validate_prepare(claims.tenant_id, None, &operation).is_err());
        claims
            .permission_scopes
            .insert("onboarding:manage".into(), "SELF".into());
        assert!(validate_prepare(claims.tenant_id, Some(&claims), &operation).is_err());
        claims
            .permission_scopes
            .insert("onboarding:manage".into(), "ALL".into());
        operation.actor_id = Uuid::new_v4();
        assert!(validate_prepare(claims.tenant_id, Some(&claims), &operation).is_err());
        operation.actor_id = claims.sub;
        operation.state = HrOperationState::Committed;
        assert!(validate_prepare(claims.tenant_id, Some(&claims), &operation).is_err());
        operation.state = HrOperationState::Registered;
        operation.authorization_id = Some(Uuid::new_v4());
        assert!(validate_prepare(claims.tenant_id, Some(&claims), &operation).is_err());
    }
}

pub(crate) async fn apply(
    db: &DatabaseConnection,
    tenant: Uuid,
    claims: Option<&ClientClaims>,
    action: FinanceAction,
    operation: &ExitOperationStatus,
) -> KabiPayResult<ExitAuthorization> {
    let input = registered(operation);
    let actor = if action == FinanceAction::PrepareExitApproval {
        Some(validate_prepare(tenant, claims, operation)?)
    } else {
        None
    };
    let tx = db.begin().await?;
    let result = if let Some(actor) = actor {
        crate::services::loan_recovery::lock(&tx, tenant).await?;
        let review = kabipay_loans::find_exit_review(&tx, &actor, input.review_id).await.map_err(financial_error)?;
        super::decision_authority::verify(&tx, tenant, input.employee_id, &review.decision).await?;
        kabipay_loans::authorize_exit_approval(&tx, &actor, &input).await
    } else {
        kabipay_loans::reconcile_exit_approval(&tx, tenant, &input, Some(operation.state)).await
    }
    .map_err(financial_error)?;
    if result.state == FinanceAuthorizationState::Confirmed {
        if operation.authorization_id != Some(result.id) {
            return Err(KabiPayError::Conflict(
                "HR confirmation has a different authorization reference".into(),
            ));
        }
        super::source::ensure_draft(&tx, tenant, operation.separation_id).await?;
    }
    tx.commit().await?;
    Ok(result)
}

pub(crate) fn financial_error(error: kabipay_loans::LoanModuleError) -> KabiPayError {
    use kabipay_loans::LoanModuleError;
    match error {
        LoanModuleError::Authority(error) => error,
        LoanModuleError::Forbidden => {
            KabiPayError::Forbidden("Loan finance authority is required".into())
        }
        LoanModuleError::NotFound => KabiPayError::NotFound {
            entity: "exit_finance_review",
            id: "reviewed operation".into(),
        },
        LoanModuleError::VersionConflict => {
            KabiPayError::Conflict("Exit or loan information changed; review it again".into())
        }
        LoanModuleError::ExitReviewPending => {
            KabiPayError::Conflict("An HR exit approval requires reconciliation".into())
        }
        LoanModuleError::Database(_) | LoanModuleError::Serialization(_) => {
            KabiPayError::Internal("Exit finance coordination could not complete".into())
        }
        _ => KabiPayError::Conflict("Exit finance inputs require review".into()),
    }
}
