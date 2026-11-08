use crate::resolvers::{MutationRoot, QueryRoot};
use async_graphql::{EmptySubscription, Schema};
use axum::{
    extract::{DefaultBodyLimit, State},
    routing::post,
    Json, Router,
};
use kabipay_common::{
    db::{resolve_required_tenant_db, TenantDbCache, TenantDbConfig},
    finance_contract::{
        self, ExitOperationStatus, ExitSubject, FinanceAction, ServiceCaller, SignedFinanceMessage,
    },
    finance_transport,
    subgraph::ensure_active_client_user,
    KabiPayError, KabiPayResult,
};
use sea_orm::{DatabaseConnection, TransactionTrait};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

pub(super) type PayrollSchema = Schema<QueryRoot, MutationRoot, EmptySubscription>;

pub fn routes() -> Router<Arc<PayrollSchema>> {
    Router::new()
        .route("/internal/finance", post(handle))
        .layer(DefaultBodyLimit::max(131072))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationInput {
    operation_id: Uuid,
}
#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeparationInput {
    separation_id: Uuid,
}

pub(super) async fn hr_read(
    tenant: Uuid,
    action: FinanceAction,
    input: serde_json::Value,
) -> KabiPayResult<serde_json::Value> {
    finance_transport::send(
        ServiceCaller::Payroll,
        ServiceCaller::Employee,
        tenant,
        None,
        action,
        input,
    )
    .await
}

async fn handle(
    State(schema): State<Arc<PayrollSchema>>,
    Json(envelope): Json<SignedFinanceMessage>,
) -> Result<Json<finance_contract::SignedFinanceResponse>, KabiPayError> {
    let secret = finance_contract::service_secret()?;
    let message = finance_contract::verify_message(
        &envelope,
        secret.as_bytes(),
        ServiceCaller::Payroll,
        chrono::Utc::now().timestamp(),
    )?;
    let ops = schema
        .data::<DatabaseConnection>()
        .ok_or_else(|| KabiPayError::Internal("Finance database unavailable".into()))?;
    kabipay_common::entitlements::require_current_module(ops, message.tenant_id, "EMPLOYEE")
        .await?;
    if let Some(claims) = &message.client_claims {
        ensure_active_client_user(&schema, message.tenant_id, claims.sub).await?;
    }
    let cache = schema
        .data::<TenantDbCache>()
        .ok_or_else(|| KabiPayError::Internal("Finance database cache unavailable".into()))?;
    let fallback = schema.data::<TenantDbConfig>().ok_or_else(|| {
        KabiPayError::Internal("Finance database configuration unavailable".into())
    })?;
    let db = resolve_required_tenant_db(message.tenant_id, ops, cache, fallback).await?;
    let result = if matches!(
        message.action,
        FinanceAction::EnsureFnf | FinanceAction::PrepareEmptyExitReview
    ) {
        let input: SeparationInput = serde_json::from_value(message.input.clone())
            .map_err(|_| KabiPayError::Validation("Invalid separation reference".into()))?;
        let subject: ExitSubject = serde_json::from_value(
            hr_read(
                message.tenant_id,
                FinanceAction::ReadSeparation,
                serde_json::json!({"separationId":input.separation_id}),
            )
            .await?,
        )
        .map_err(|_| KabiPayError::Conflict("HR separation could not be verified".into()))?;
        let expected_status = if message.action == FinanceAction::EnsureFnf {
            "APPROVED"
        } else {
            "PENDING"
        };
        if subject.separation_id != input.separation_id || subject.status != expected_status {
            return Err(KabiPayError::Conflict(
                "HR separation status does not permit this action".into(),
            ));
        }
        if message.action == FinanceAction::PrepareEmptyExitReview {
            let claims = message
                .client_claims
                .as_ref()
                .ok_or(KabiPayError::Unauthorised)?;
            let value =
                super::empty_review::prepare(&db, ops, message.tenant_id, claims, &subject).await?;
            return Ok(Json(finance_contract::signed_response(
                message,
                value,
                chrono::Utc::now().timestamp(),
                secret.as_bytes(),
            )?));
        }
        let tx = db.begin().await?;
        let fnf =
            super::source::ensure_draft(&tx, message.tenant_id, subject.separation_id).await?;
        tx.commit().await?;
        serde_json::json!({"fnfId":fnf.id,"separationId":subject.separation_id})
    } else {
        let input: OperationInput = serde_json::from_value(message.input.clone())
            .map_err(|_| KabiPayError::Validation("Invalid HR operation reference".into()))?;
        let operation: ExitOperationStatus = serde_json::from_value(
            hr_read(
                message.tenant_id,
                FinanceAction::OperationStatus,
                serde_json::json!({"operationId":input.operation_id}),
            )
            .await?,
        )
        .map_err(|_| {
            KabiPayError::Conflict(
                "Authoritative HR operation is unavailable; approval remains held".into(),
            )
        })?;
        if operation.id != input.operation_id {
            return Err(KabiPayError::Forbidden(
                "HR operation reference does not match".into(),
            ));
        }
        if operation.state == finance_contract::HrOperationState::Cancelled {
            let tx = db.begin().await?;
            let exists = kabipay_loans::exit_authorization(&tx, message.tenant_id, operation.id)
                .await
                .map_err(super::coordination::financial_error)?
                .is_some();
            tx.rollback().await?;
            if !exists {
                let value = serde_json::json!({"operationId":operation.id,"state":"CANCELLED","cancelledWithoutAuthorization":true});
                return Ok(Json(finance_contract::signed_response(
                    message,
                    value,
                    chrono::Utc::now().timestamp(),
                    secret.as_bytes(),
                )?));
            }
        }
        serde_json::to_value(
            super::coordination::apply(
                &db,
                message.tenant_id,
                message.client_claims.as_ref(),
                message.action,
                &operation,
            )
            .await?,
        )
        .map_err(|_| KabiPayError::Internal("Exit authorization serialization failed".into()))?
    };
    Ok(Json(finance_contract::signed_response(
        message,
        result,
        chrono::Utc::now().timestamp(),
        secret.as_bytes(),
    )?))
}
