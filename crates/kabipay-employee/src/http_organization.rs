use super::EmployeeState;
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Router,
};
use std::sync::Arc;

pub(super) fn routes() -> Router<Arc<EmployeeState>> {
    Router::new()
        .route(
            "/internal/organization/command",
            post(
                |State(st): State<Arc<EmployeeState>>, headers: HeaderMap, body: Bytes| async move {
                    let (tenant, db) = kabipay_organization_tenant::fixed_database(
                        &st.cache,
                        &st.ops,
                        &st.fallback,
                    )
                    .await
                    .map_err(|_| {
                        (
                            StatusCode::SERVICE_UNAVAILABLE,
                            "Organization runtime requires a fixed company binding".to_owned(),
                        )
                    })?;
                    kabipay_organization_tenant::handle(
                        kabipay_employee::organization::EmployeeAdapter,
                        tenant,
                        db,
                        &st.ops,
                        headers,
                        body,
                    )
                    .await
                },
            ),
        )
        .layer(DefaultBodyLimit::max(65536))
}

pub(super) async fn bootstrap_worker(st: Arc<EmployeeState>) {
    if st.cache.bound_tenant_id().is_none() {
        return;
    }
    let mut interval = tokio::time::interval(std::time::Duration::from_secs(2));
    let mut capture_at = std::time::Instant::now();
    loop {
        interval.tick().await;
        let capture_due = std::time::Instant::now() >= capture_at;
        let result = async {
            let (tenant, db) =
                kabipay_organization_tenant::fixed_database(&st.cache, &st.ops, &st.fallback)
                    .await?;
            kabipay_organization_tenant::bootstrap_once(&db, tenant, 100).await?;
            if capture_due {
                kabipay_organization_tenant::capture_once(&db, tenant).await?;
            }
            Ok::<_, kabipay_common::KabiPayError>(())
        }
        .await;
        if result.is_ok() && capture_due {
            capture_at = std::time::Instant::now() + std::time::Duration::from_secs(30);
        }
        if let Err(error) = result {
            tracing::warn!(error=%error,"source organization bootstrap batch deferred");
        }
    }
}
