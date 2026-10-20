//! Explicit metadata connection and trusted source endpoints; no tenant routing.
use crate::{
    graphql::{schema_builder, OrganizationSchema},
    identity::IdentityClient,
    store::Store,
};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::DefaultBodyLimit,
    extract::State,
    http::{HeaderMap, StatusCode},
    routing::{get, post},
    Router,
};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;
#[derive(Clone)]
pub struct OrganizationState {
    pub schema: Arc<OrganizationSchema>,
    pub identities: IdentityClient,
    pub execution_enabled: bool,
}
pub fn identity_client_from_env() -> anyhow::Result<IdentityClient> {
    let value = std::env::var("KABIPAY_ORGANIZATION_IDENTITY_ENDPOINTS")
        .map_err(|_| anyhow::anyhow!("organization identity endpoints are required"))?;
    let endpoints: HashMap<Uuid, String> = serde_json::from_str(&value)
        .map_err(|_| anyhow::anyhow!("invalid organization identity endpoint map"))?;
    IdentityClient::new(endpoints).map_err(Into::into)
}
pub async fn store_from_env() -> anyhow::Result<Store> {
    if std::env::var_os("KABIPAY_BOUND_DATABASE_URL").is_some() {
        anyhow::bail!("organization service must not receive a tenant database binding");
    }
    let url = std::env::var("KABIPAY_ORGANIZATION_DATABASE_URL")
        .map_err(|_| anyhow::anyhow!("explicit organization metadata database URL is required"))?;
    let parsed =
        reqwest::Url::parse(&url).map_err(|_| anyhow::anyhow!("invalid metadata database URL"))?;
    if !matches!(parsed.scheme(), "postgres" | "postgresql")
        || parsed.host_str().is_none()
        || parsed.username().is_empty()
        || parsed.path().trim_start_matches('/').is_empty()
    {
        anyhow::bail!("invalid metadata database URL");
    }
    let db = kabipay_common::db::connect_ops_db(&url)
        .await
        .map_err(|_| anyhow::anyhow!("organization metadata database connection failed"))?;
    Ok(Store::new(db))
}
pub fn router(store: Store, identities: IdentityClient) -> Router {
    router_with_execution(store, identities, None)
}
pub fn router_with_execution(
    store: Store,
    identities: IdentityClient,
    execution: Option<crate::execution::Execution>,
) -> Router {
    let mut builder = schema_builder(store);
    if let Some(engine) = execution.clone() {
        builder = builder.data(engine);
    }
    let state = OrganizationState {
        schema: Arc::new(builder.finish()),
        identities,
        execution_enabled: execution.is_some(),
    };
    let public = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route("/graphql", post(graphql_post))
        .layer(DefaultBodyLimit::max(65536))
        .with_state(state);
    if let Some(engine) = execution {
        let internal = Router::new()
            .route(
                "/internal/organization/payroll-jobs/authorize",
                post(crate::payroll_job_policy::authorize),
            )
            .route(
                "/internal/organization/memberships/ack",
                post(crate::policy_endpoint::membership_ack),
            )
            .route(
                "/internal/organization/policy",
                post(crate::policy_endpoint::policy),
            )
            .route(
                "/internal/organization/grants/ack",
                post(crate::policy_endpoint::grant_ack),
            )
            .route(
                "/internal/organization/relationships/ack",
                post(crate::policy_endpoint::relationship_ack),
            )
            .layer(DefaultBodyLimit::max(131072))
            .with_state(engine);
        public.merge(internal)
    } else {
        public
    }
}
pub async fn graphql_post(
    State(state): State<OrganizationState>,
    headers: HeaderMap,
    request: GraphQLRequest,
) -> Result<GraphQLResponse, (StatusCode, String)> {
    let mut request = request.into_inner();
    if let Some(bearer) = headers.get(axum::http::header::AUTHORIZATION) {
        let tenant = headers
            .get("x-login-company-id")
            .and_then(|v| v.to_str().ok())
            .and_then(|v| Uuid::parse_str(v).ok())
            .ok_or((
                StatusCode::UNAUTHORIZED,
                "source company selector is required".into(),
            ))?;
        let bearer = bearer
            .to_str()
            .map_err(|_| (StatusCode::UNAUTHORIZED, "invalid authorization".into()))?;
        let identity = state
            .identities
            .verify(tenant, bearer)
            .await
            .map_err(|error| (error.http_status(), error.to_string()))?;
        if state.execution_enabled && !identity.asymmetric() {
            return Err((
                StatusCode::UNAUTHORIZED,
                "organization execution requires company-bound RSA client authentication".into(),
            ));
        }
        request = request.data(identity);
    }
    let response = tokio::time::timeout(
        std::time::Duration::from_secs(45),
        state.schema.execute(request),
    )
    .await
    .map_err(|_| {
        (
            StatusCode::GATEWAY_TIMEOUT,
            "organization request timed out".into(),
        )
    })?;
    Ok(response.into())
}
