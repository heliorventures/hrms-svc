//! HR commits its approval only after a durable Finance authorization. No financial writes.
use super::{finance_operation, offboarding_fnf_service};
use chrono::NaiveDate;
use kabipay_common::{
    context::ClientClaims,
    finance_contract::{
        ExitFinanceAuthorization, FinanceAction, FinanceAuthorizationState, HrOperationState,
        ServiceCaller,
    },
    finance_transport, KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0017_onboarding_offboarding::separation;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use uuid::Uuid;

async fn finance(
    tenant: Uuid,
    claims: Option<&ClientClaims>,
    action: FinanceAction,
    input: serde_json::Value,
) -> KabiPayResult<serde_json::Value> {
    finance_transport::send(
        ServiceCaller::Employee,
        ServiceCaller::Payroll,
        tenant,
        claims,
        action,
        input,
    )
    .await
}

pub async fn approve(
    db: &DatabaseConnection,
    tenant: Uuid,
    separation_id: Uuid,
    claims: &ClientClaims,
    review_id: Option<Uuid>,
    business_date: NaiveDate,
) -> KabiPayResult<separation::Model> {
    if claims.tenant_id != tenant
        || claims.iss != kabipay_common::context::CLIENT_JWT_ISSUER
        || kabipay_common::client_data_scope::data_scope_from_claims(
            Some(claims),
            "onboarding:manage",
        )? != kabipay_common::context::ScopeType::All
    {
        return Err(KabiPayError::Forbidden(
            "Exit approval requires ALL HR scope".into(),
        ));
    }
    let row = offboarding_fnf_service::get_separation_tenant(db, tenant, separation_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "separation",
            id: separation_id.to_string(),
        })?;
    let prior = finance_operation::for_separation(db, tenant, separation_id, claims.sub).await?;
    if row.status == "APPROVED"
        && prior.as_ref().is_some_and(|operation| {
            operation.state == HrOperationState::Committed
                && review_id.map_or(true, |id| id == operation.review_id)
        })
    {
        return Ok(row);
    }
    if row.status != "PENDING" {
        return Err(KabiPayError::Conflict(
            "Separation is no longer pending; refresh its recorded outcome".into(),
        ));
    }
    let review = match review_id.or_else(|| prior.map(|operation| operation.review_id)) {
        Some(id) if !id.is_nil() => id,
        Some(_) => {
            return Err(KabiPayError::Validation(
                "Exit review reference is required".into(),
            ))
        }
        None => {
            let result = finance(
                tenant,
                Some(claims),
                FinanceAction::PrepareEmptyExitReview,
                serde_json::json!({"separationId":separation_id}),
            )
            .await?;
            serde_json::from_value(result.get("reviewId").cloned().ok_or_else(|| {
                KabiPayError::Conflict("Finance review could not be verified".into())
            })?)
            .map_err(|_| KabiPayError::Conflict("Finance review reference is invalid".into()))?
        }
    };
    let operation = finance_operation::register(db, tenant, claims, &row, review).await?;
    let result = finance(
        tenant,
        Some(claims),
        FinanceAction::PrepareExitApproval,
        serde_json::json!({"operationId":operation.id}),
    )
    .await?;
    let authorization: ExitFinanceAuthorization = serde_json::from_value(result).map_err(|_| {
        KabiPayError::Conflict("Finance authorization could not be verified".into())
    })?;
    validate_authorization(&operation, &authorization)?;
    match commit(db, tenant, &operation, &authorization, business_date).await {
        Ok(row) => {
            // The outbox is already committed. An unavailable Finance service cannot undo HR.
            let _ = finance(
                tenant,
                None,
                FinanceAction::ConfirmExitApproval,
                serde_json::json!({"operationId":operation.id}),
            )
            .await;
            Ok(row)
        }
        Err(error) => {
            finance_operation::cancel(db, tenant, operation.id).await?;
            let _ = finance(
                tenant,
                None,
                FinanceAction::CancelExitApproval,
                serde_json::json!({"operationId":operation.id}),
            )
            .await;
            Err(error)
        }
    }
}

fn validate_authorization(
    operation: &finance_operation::ExitOperation,
    authorization: &ExitFinanceAuthorization,
) -> KabiPayResult<()> {
    if authorization.id.is_nil()
        || authorization.operation_id != operation.id
        || authorization.review_id != operation.review_id
        || authorization.employee_id != operation.employee_id
        || authorization.separation_id != operation.separation_id
        || authorization.actor_id != operation.actor_id
        || authorization.payload_hash != operation.payload_hash
        || authorization.state != FinanceAuthorizationState::Authorized
    {
        return Err(KabiPayError::Conflict(
            "Finance authorization does not match the registered HR approval".into(),
        ));
    }
    Ok(())
}

async fn commit(
    db: &DatabaseConnection,
    tenant: Uuid,
    expected: &finance_operation::ExitOperation,
    authorization: &ExitFinanceAuthorization,
    business_date: NaiveDate,
) -> KabiPayResult<separation::Model> {
    let tx = db.begin().await?;
    let row = separation::Entity::find_by_id(expected.separation_id)
        .filter(separation::Column::TenantId.eq(tenant))
        .lock_exclusive()
        .one(&tx)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "separation",
            id: expected.separation_id.to_string(),
        })?;
    let operation = finance_operation::find(&tx, tenant, expected.id, true)
        .await?
        .ok_or_else(|| KabiPayError::Conflict("HR approval operation is unavailable".into()))?;
    if operation.state == HrOperationState::Committed
        && row.status == "APPROVED"
        && operation.authorization_id == Some(authorization.id)
    {
        tx.commit().await?;
        return Ok(row);
    }
    validate_authorization(&operation, authorization)?;
    if operation.state != HrOperationState::Registered
        || row.status != "PENDING"
        || finance_operation::subject(&row)?.fingerprint != operation.separation_fingerprint
    {
        return Err(KabiPayError::Conflict(
            "Separation changed after the financial review".into(),
        ));
    }
    let now = chrono::Utc::now();
    let mut active: separation::ActiveModel = row.into();
    active.status = Set("APPROVED".into());
    active.approved_by = Set(Some(operation.actor_id));
    active.updated_at = Set(now);
    let row = active.update(&tx).await?;
    offboarding_fnf_service::ensure_clearance_on_approval(&tx, tenant, row.id).await?;
    kabipay_common::due_offboarding::offboard_approved_separation_in_transaction(
        &tx,
        tenant,
        row.id,
        business_date,
        now,
    )
    .await?;
    finance_operation::record_terminal(
        &tx,
        tenant,
        &operation,
        HrOperationState::Committed,
        Some(authorization.id),
    )
    .await?;
    tx.commit().await?;
    offboarding_fnf_service::get_separation_tenant(db, tenant, row.id)
        .await?
        .ok_or_else(|| KabiPayError::Internal("Approved separation could not be read".into()))
}
