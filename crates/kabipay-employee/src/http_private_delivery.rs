//! This route is private-service authenticated before any tenant database is resolved.
use super::EmployeeState;
use axum::{
    extract::{DefaultBodyLimit, State},
    routing::post,
    Json, Router,
};
use kabipay_common::{db::resolve_required_tenant_db, KabiPayError};
use kabipay_employee::services::private_delivery::{
    self, SignedDeliveryRequest, SignedDeliveryResponse,
};
use kabipay_employee::services::private_ownership::{
    self, SignedOwnershipRequest, SignedOwnershipResponse,
};
use std::sync::Arc;

pub(super) fn routes() -> Router<Arc<EmployeeState>> {
    Router::new()
        .route("/internal/private-events/delivery", post(handle))
        .route("/internal/private-events/ownership", post(ownership))
        .layer(DefaultBodyLimit::max(16384))
}
async fn ownership(
    State(state): State<Arc<EmployeeState>>,
    Json(envelope): Json<SignedOwnershipRequest>,
) -> Result<Json<SignedOwnershipResponse>, KabiPayError> {
    private_ownership::authorize(&envelope)?;
    let db = resolve_required_tenant_db(
        envelope.request.tenant_id,
        &state.ops,
        &state.cache,
        &state.fallback,
    )
    .await?;
    Ok(Json(private_ownership::acknowledge(&db, &envelope).await?))
}
async fn handle(
    State(state): State<Arc<EmployeeState>>,
    Json(envelope): Json<SignedDeliveryRequest>,
) -> Result<Json<SignedDeliveryResponse>, KabiPayError> {
    private_delivery::authorize(&envelope)?;
    let db = resolve_required_tenant_db(
        envelope.request.tenant_id,
        &state.ops,
        &state.cache,
        &state.fallback,
    )
    .await?;
    Ok(Json(
        private_delivery::execute(&db, &state.ops, &envelope).await?,
    ))
}
