//! Mail-only private endpoints. Response/error bodies never enter public APIs.
use super::EmployeeState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use kabipay_common::{important_email::*, KabiPayError};
use std::sync::Arc;

fn failure(error: KabiPayError) -> (StatusCode, &'static str) {
    (
        match error {
            KabiPayError::Unauthorised => StatusCode::UNAUTHORIZED,
            KabiPayError::Forbidden(_) => StatusCode::FORBIDDEN,
            _ => StatusCode::SERVICE_UNAVAILABLE,
        },
        "Email authority unavailable",
    )
}
pub(super) fn routes() -> Router<Arc<EmployeeState>> {
    Router::new()
        .route(
            EMAIL_ELIGIBILITY_ROUTE,
            post(
                |State(st): State<Arc<EmployeeState>>, headers: HeaderMap, body: Bytes| async move {
                    kabipay_organization_tenant::email_routes::domain_eligibility(
                        kabipay_employee::email_alerts::EmployeeEmailAdapter,
                        &st.cache,
                        &st.ops,
                        &st.fallback,
                        &headers,
                        &body,
                    )
                    .await
                    .map(Json)
                    .map_err(failure)
                },
            ),
        )
        .route(
            EMAIL_RECIPIENT_ROUTE,
            post(
                |State(st): State<Arc<EmployeeState>>, headers: HeaderMap, body: Bytes| async move {
                    kabipay_organization_tenant::email_routes::recipient(
                        &st.cache,
                        &st.ops,
                        &st.fallback,
                        &headers,
                        &body,
                    )
                    .await
                    .map(Json)
                    .map_err(failure)
                },
            ),
        )
        .layer(DefaultBodyLimit::max(EMAIL_PRIVATE_BODY_LIMIT))
}
