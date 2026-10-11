//! Service-authenticated read side of the durable HR approval handshake.
use super::EmployeeState;
use axum::{
    extract::{DefaultBodyLimit, State},
    routing::post,
    Json, Router,
};
use kabipay_common::{
    db::resolve_required_tenant_db,
    finance_contract::{self, FinanceAction, ServiceCaller, SignedFinanceMessage},
    KabiPayError,
};
use kabipay_employee::services::{finance_operation, offboarding_fnf_service};
use serde::Deserialize;
use std::sync::Arc;
use uuid::Uuid;

pub(super) fn routes() -> Router<Arc<EmployeeState>> {
    Router::new()
        .route("/internal/finance", post(handle))
        .layer(DefaultBodyLimit::max(131072))
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct SeparationInput {
    separation_id: Uuid,
    #[serde(default)]
    public_read: bool,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct OperationInput {
    operation_id: Uuid,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
struct FundingInput {
    employee_id: Uuid,
    value_date: chrono::NaiveDate,
}

async fn handle(
    State(state): State<Arc<EmployeeState>>,
    Json(envelope): Json<SignedFinanceMessage>,
) -> Result<Json<finance_contract::SignedFinanceResponse>, KabiPayError> {
    let secret = finance_contract::service_secret()?;
    let message = finance_contract::verify_message(
        &envelope,
        secret.as_bytes(),
        ServiceCaller::Employee,
        chrono::Utc::now().timestamp(),
    )?;
    kabipay_common::entitlements::require_current_module(&state.ops, message.tenant_id, "EMPLOYEE")
        .await?;
    if let Some(claims) = &message.client_claims {
        kabipay_common::subgraph::ensure_active_client_user(
            state.schema.as_ref(),
            message.tenant_id,
            claims.sub,
        )
        .await?;
    }
    let db =
        resolve_required_tenant_db(message.tenant_id, &state.ops, &state.cache, &state.fallback)
            .await?;
    let value = match message.action {
        FinanceAction::ReadFundingContext => {
            let claims = message
                .client_claims
                .as_ref()
                .ok_or(KabiPayError::Unauthorised)?;
            if kabipay_common::client_data_scope::data_scope_from_claims(
                Some(claims),
                "loan:disburse",
            )? != kabipay_common::context::ScopeType::All
            {
                return Err(KabiPayError::Forbidden(
                    "Funding status requires company loan disbursement authority".into(),
                ));
            }
            let input: FundingInput = serde_json::from_value(message.input.clone())
                .map_err(|_| KabiPayError::Validation("Invalid funding reference".into()))?;
            let clock = kabipay_common::tenant_business_clock::TenantBusinessClock::load(
                &state.ops,
                message.tenant_id,
            )
            .await?;
            let context = kabipay_employee::services::funding_context::read(
                &db,
                message.tenant_id,
                input.employee_id,
                input.value_date,
                clock,
            )
            .await?;
            serde_json::to_value(context).map_err(KabiPayError::from)?
        }
        FinanceAction::ReadSeparation => {
            let input: SeparationInput = serde_json::from_value(message.input.clone())
                .map_err(|_| KabiPayError::Validation("Invalid separation reference".into()))?;
            let employee = if input.public_read {
                finance_contract::exit_visibility_employee(
                    message
                        .client_claims
                        .as_ref()
                        .ok_or(KabiPayError::Unauthorised)?,
                )?
            } else {
                None
            };
            let row = offboarding_fnf_service::get_visible_separation(
                &db,
                message.tenant_id,
                input.separation_id,
                employee,
            )
            .await?;
            if input.public_read {
                let subject = row.as_ref().map(finance_operation::subject).transpose()?;
                return Ok(Json(finance_contract::signed_response(
                    message,
                    serde_json::to_value(subject).map_err(KabiPayError::from)?,
                    chrono::Utc::now().timestamp(),
                    secret.as_bytes(),
                )?));
            }
            let row = row.ok_or_else(|| KabiPayError::NotFound {
                entity: "separation",
                id: input.separation_id.to_string(),
            })?;
            serde_json::to_value(finance_operation::subject(&row)?)
                .map_err(|_| KabiPayError::Internal("Exit subject serialization failed".into()))?
        }
        FinanceAction::OperationStatus => {
            let input: OperationInput = serde_json::from_value(message.input.clone())
                .map_err(|_| KabiPayError::Validation("Invalid exit operation reference".into()))?;
            match finance_operation::find(&db, message.tenant_id, input.operation_id, false).await?
            {
                Some(operation) => serde_json::to_value(operation).map_err(|_| {
                    KabiPayError::Internal("Exit operation serialization failed".into())
                })?,
                None => serde_json::json!({"state":"UNKNOWN","id":input.operation_id}),
            }
        }
        _ => return Err(KabiPayError::Forbidden("Invalid private HR action".into())),
    };
    Ok(Json(finance_contract::signed_response(
        message,
        value,
        chrono::Utc::now().timestamp(),
        secret.as_bytes(),
    )?))
}
