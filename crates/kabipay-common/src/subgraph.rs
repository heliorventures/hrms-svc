//! Shared scaffolding for federated GraphQL subgraphs.
//!
//! Every `kabipay-*` subgraph crate boots essentially the same way:
//! connect to the ops DB, open a tenant DB cache, expose `/graphql` +
//! `/healthz`, and inject a tenant id into the request context from an
//! `x-tenant-id` header (until `kabipay-auth` issues real JWTs).
//!
//! This module provides [`serve_subgraph`], a one-liner that wires all of
//! the above given a pre-built GraphQL schema. Each subgraph's `main.rs`
//! only has to build its `Schema<QueryRoot, …>` and call us.
//!
//! Resolvers obtain the caller's tenant id and SeaORM connection via the
//! [`require_tenant_id`] and [`tenant_db`] helpers in this module.

use crate::context::{ClientClaims, ClientRequestHints, OperatorClaims, OperatorContext};
use crate::db::{
    apply_postgres_ssl_mode_to_url, connect_ops_db, resolve_tenant_db, TenantDbCache,
    TenantDbConfig,
};
use crate::error::{KabiPayError, KabiPayResult};
use crate::jwt::{decode_client_jwt, decode_operator_jwt, extract_bearer, jwt_secret_from_env};
use crate::telemetry::init_tracing;
use async_graphql::{Context, ObjectType, Schema, SubscriptionType};
use async_graphql_axum::{GraphQLRequest, GraphQLResponse};
use axum::{
    extract::State,
    http::{header, HeaderMap, StatusCode},
    response::IntoResponse,
    routing::get,
    Router,
};
use kabipay_db_entities::tenant::{d0005_auth_rbac::user, d0007_employee_core::employee};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::{net::SocketAddr, sync::Arc};
use tower_http::{cors::CorsLayer, trace::TraceLayer};
use uuid::Uuid;

/// Dev-only HTTP header used to propagate the current tenant id when no
/// `Authorization: Bearer <jwt>` is present. Production deployments should
/// NEVER rely on this header; set `KABIPAY_REQUIRE_AUTH=1` to reject
/// requests that try to use it.
pub const TENANT_HEADER: &str = "x-tenant-id";

/// Newtype marker inserted into the GraphQL request context once the
/// middleware has validated a JWT (or accepted the dev header).
/// Resolvers read it via [`require_tenant_id`].
#[derive(Clone, Copy, Debug)]
pub struct TenantId(pub Uuid);

/// Build a `postgres://` DSN for the ops plane from standard env vars.
///
/// Honours `DATABASE_URL` when set; otherwise falls back to the individual
/// `POSTGRES_*` vars with safe defaults aligned with `docker-compose.yml`.
pub fn ops_dsn_from_env() -> String {
    let base = if let Ok(url) = std::env::var("DATABASE_URL") {
        url
    } else {
        let host = std::env::var("POSTGRES_HOST").unwrap_or_else(|_| "localhost".into());
        let port = std::env::var("POSTGRES_PORT").unwrap_or_else(|_| "15432".into());
        let db = std::env::var("POSTGRES_DB").unwrap_or_else(|_| "kabipay_dev".into());
        let user = std::env::var("POSTGRES_USER").unwrap_or_else(|_| "kabipay".into());
        let pass = std::env::var("POSTGRES_PASSWORD").unwrap_or_else(|_| "changeme".into());
        format!("postgres://{user}:{pass}@{host}:{port}/{db}")
    };
    apply_postgres_ssl_mode_to_url(&base)
}

/// Build a [`TenantDbConfig`] used as fallback when
/// `kabipay_ops.tenant_database` lookup is not yet wired.
pub fn tenant_db_config_from_env() -> TenantDbConfig {
    TenantDbConfig {
        db_host: std::env::var("POSTGRES_HOST").unwrap_or_else(|_| "localhost".into()),
        db_port: std::env::var("POSTGRES_PORT")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(15432),
        db_name: std::env::var("POSTGRES_DB").unwrap_or_else(|_| "kabipay_dev".into()),
        db_user: std::env::var("POSTGRES_USER").unwrap_or_else(|_| "kabipay".into()),
        db_password: std::env::var("POSTGRES_PASSWORD").unwrap_or_else(|_| "changeme".into()),
        schema_name: String::new(),
    }
}

/// Read the caller's tenant id from the GraphQL context.
///
/// Returns a GraphQL `UNAUTHENTICATED` error if neither a JWT nor the dev
/// `x-tenant-id` header was present. Tenant-plane resolvers should ALWAYS
/// call this before hitting the DB.
pub fn require_tenant_id(ctx: &Context<'_>) -> async_graphql::Result<Uuid> {
    ctx.data_opt::<TenantId>()
        .map(|t| t.0)
        .ok_or_else(|| KabiPayError::Unauthorised.into_graphql())
}

/// Gateway-derived request metadata (client IP, etc.). Missing when a subgraph bypasses
/// [`tenant_graphql_post`]; treat as empty.
pub fn client_request_hints(ctx: &Context<'_>) -> ClientRequestHints {
    ctx.data_opt::<ClientRequestHints>()
        .cloned()
        .unwrap_or_default()
}

fn client_ip_from_headers(headers: &HeaderMap) -> Option<String> {
    if let Some(raw) = headers.get("x-forwarded-for").and_then(|v| v.to_str().ok()) {
        if let Some(first) = raw.split(',').next() {
            let s = first.trim();
            if !s.is_empty() {
                return Some(s.to_string());
            }
        }
    }
    headers
        .get("x-real-ip")
        .and_then(|v| v.to_str().ok())
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
}

fn request_id_from_headers(headers: &HeaderMap) -> Result<Option<String>, (StatusCode, String)> {
    let Some(value) = headers.get("x-request-id") else {
        return Ok(None);
    };
    let value = value.to_str().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            "x-request-id header is not valid ASCII".to_string(),
        )
    })?;
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    Ok(Some(value.chars().take(128).collect()))
}

/// Read the caller's full JWT claims. Available only when the caller
/// authenticated with `Authorization: Bearer <jwt>` (not the dev header).
/// Resolvers that need `user_id`, `roles`, or `permissions` should call
/// this; resolvers that only need `tenant_id` should prefer
/// [`require_tenant_id`].
pub fn require_client_claims<'a>(ctx: &'a Context<'a>) -> async_graphql::Result<&'a ClientClaims> {
    ctx.data_opt::<ClientClaims>()
        .ok_or_else(|| KabiPayError::Unauthorised.into_graphql())
}

/// Resolves the caller's employee row id. Uses [`ClientClaims::employee_id`]
/// when set (see `kabipay-auth`); otherwise looks up
/// `employee.user_id = claims.sub` in the tenant schema. Fails on the
/// `x-tenant-id`-only dev path (no JWT) with [`KabiPayError::Unauthorised`].
pub async fn resolve_client_employee_id(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Uuid> {
    resolve_client_employee_id_with_connection(ctx, db, tenant_id).await
}

/// Connection-generic form of [`resolve_client_employee_id`] for callers that
/// must resolve an employee inside their existing transaction.
pub async fn resolve_client_employee_id_with_connection<C>(
    ctx: &Context<'_>,
    db: &C,
    tenant_id: Uuid,
) -> KabiPayResult<Uuid>
where
    C: ConnectionTrait + Sync,
{
    let claims = ctx
        .data_opt::<ClientClaims>()
        .ok_or(KabiPayError::Unauthorised)?;
    if let Some(eid) = claims.employee_id {
        return Ok(eid);
    }
    let row = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::UserId.eq(claims.sub))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await?;
    let Some(m) = row else {
        return Err(KabiPayError::Validation(
            "no employee profile linked to this user".into(),
        ));
    };
    Ok(m.id)
}

/// Department and location ids for the caller's **employee** profile, when present.
///
/// Returns `Ok(None)` when the user has no linked employee row (e.g. service account) or JWT-only dev path.
pub async fn try_client_employee_dept_and_location(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    claims: &ClientClaims,
) -> KabiPayResult<Option<(Option<Uuid>, Option<Uuid>)>> {
    let emp = if let Some(eid) = claims.employee_id {
        employee::Entity::find_by_id(eid)
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(db)
            .await?
    } else {
        employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::UserId.eq(claims.sub))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(db)
            .await?
    };
    Ok(emp.map(|m| (m.department_id, m.location_id)))
}

/// Resolve a pooled SeaORM connection pinned to the tenant's schema.
///
/// Requires that [`serve_subgraph`] attached `TenantDbCache`,
/// `DatabaseConnection` (ops) and `TenantDbConfig` to the schema data — which
/// it does by default for every tenant-plane subgraph.
pub async fn tenant_db(
    ctx: &Context<'_>,
    tenant_id: Uuid,
) -> async_graphql::Result<DatabaseConnection> {
    let cache = ctx.data::<TenantDbCache>().map_err(|_| {
        KabiPayError::Internal("TenantDbCache missing from schema data".into()).into_graphql()
    })?;
    let ops_db = ctx.data::<DatabaseConnection>().map_err(|_| {
        KabiPayError::Internal("ops DatabaseConnection missing from schema data".into())
            .into_graphql()
    })?;
    let fallback = ctx.data::<TenantDbConfig>().map_err(|_| {
        KabiPayError::Internal("TenantDbConfig missing from schema data".into()).into_graphql()
    })?;
    resolve_tenant_db(tenant_id, ops_db, cache, fallback)
        .await
        .map_err(KabiPayError::into_graphql)
}

/// Resolve the ops plane SeaORM connection from the GraphQL context. Use
/// this in operator / tenant / billing subgraphs that read ops tables.
pub fn ops_db<'a>(ctx: &'a Context<'a>) -> async_graphql::Result<&'a DatabaseConnection> {
    ctx.data::<DatabaseConnection>().map_err(|_| {
        KabiPayError::Internal("ops DatabaseConnection missing from schema data".into())
            .into_graphql()
    })
}

/// Operator-only GraphQL resolvers must call this first.
pub fn require_operator_context<'a>(
    ctx: &'a Context<'a>,
) -> async_graphql::Result<&'a OperatorContext> {
    ctx.data_opt::<OperatorContext>().ok_or_else(|| {
        KabiPayError::Forbidden("operator JWT required (kabipay-ops)".into()).into_graphql()
    })
}

/// Configuration for [`serve_subgraph`].
pub struct SubgraphConfig<'a> {
    /// Service name used by `init_tracing`.
    pub service_name: &'a str,
    /// TCP port used when the env override is absent.
    pub default_port: u16,
    /// Env var checked for a port override (e.g. `KABIPAY_LEAVE_PORT`).
    pub port_env: &'a str,
    /// `true` to open the ops DB and tenant cache at startup. Set `false`
    /// for subgraphs that don't need DB access yet.
    pub needs_db: bool,
}

/// Build the Axum router for a pre-built GraphQL schema and serve it.
///
/// Adds:
/// - `GET /healthz` returning `"ok"`
/// - `GET /graphql` playground
/// - `POST /graphql` with tenant header extraction and schema execution
/// - CORS + structured-trace layers
///
/// If `cfg.needs_db` is true, [`connect_ops_db`] is called and
/// `DatabaseConnection`, `TenantDbCache`, `TenantDbConfig` are attached to
/// the schema data so tenant-plane resolvers can call [`tenant_db`].
pub async fn serve_subgraph<Q, M, S>(
    cfg: SubgraphConfig<'_>,
    schema_builder: async_graphql::SchemaBuilder<Q, M, S>,
) -> anyhow::Result<()>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    crate::env_file::load_dotenv();
    init_tracing(cfg.service_name);

    let port: u16 = std::env::var(cfg.port_env)
        .ok()
        .and_then(|v| v.parse().ok())
        .unwrap_or(cfg.default_port);

    let schema = if cfg.needs_db {
        let dsn = ops_dsn_from_env();
        let ops_db = connect_ops_db(&dsn).await.map_err(|e| {
            anyhow::anyhow!(
                "{}: failed to connect to ops DB: {e} (check Postgres reachability, TLS/sslmode, and KABIPAY_DB_POOL_MAX when running many services)",
                cfg.service_name
            )
        })?;
        schema_builder
            .enable_federation()
            .data(ops_db)
            .data(TenantDbCache::new())
            .data(tenant_db_config_from_env())
            .finish()
    } else {
        schema_builder.enable_federation().finish()
    };

    let state: Arc<Schema<Q, M, S>> = Arc::new(schema);

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        .route(
            "/graphql",
            get(graphql_playground).post(tenant_graphql_post::<Q, M, S>),
        )
        .with_state(state)
        .layer(CorsLayer::permissive())
        .layer(TraceLayer::new_for_http());

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, service = cfg.service_name, "subgraph listening");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    axum::serve(listener, app).await?;
    Ok(())
}

/// `POST /graphql` handler for a tenant subgraph. Exposed so binaries can mount
/// extra HTTP routes (e.g. file download) on the same listener.
pub async fn tenant_graphql_post<Q, M, S>(
    State(schema): State<Arc<Schema<Q, M, S>>>,
    headers: HeaderMap,
    req: GraphQLRequest,
) -> Result<GraphQLResponse, (StatusCode, String)>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let mut req = req.into_inner();
    let hints = ClientRequestHints {
        client_ip: client_ip_from_headers(&headers),
        request_id: request_id_from_headers(&headers)?,
    };
    req = req.data(hints);
    match extract_request_identity(&headers)? {
        RequestIdentity::Client { tenant_id, claims } => {
            req = req.data(TenantId(tenant_id));
            if let Some(c) = claims {
                ensure_active_client_user(schema.as_ref(), tenant_id, c.sub)
                    .await
                    .map_err(|error| (error.http_status(), error.to_string()))?;
                req = req.data(c);
            }
        }
        RequestIdentity::Operator(claims) => {
            let ctx = OperatorContext {
                operator_user_id: claims.sub,
                roles: claims.roles,
                tenant_access: claims.tenant_access,
            };
            req = req.data(ctx);
        }
        RequestIdentity::None => {}
    }
    Ok(schema.execute(req).await.into())
}

/// Re-check mutable account state before accepting an otherwise valid client JWT.
///
/// Access tokens remain cryptographically stateless, but account activation is
/// mutable security state. Performing this check at the shared subgraph boundary
/// makes employee deactivation effective immediately for both gateway and direct
/// subgraph traffic instead of waiting for access-token expiry.
async fn ensure_active_client_user<Q, M, S>(
    schema: &Schema<Q, M, S>,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<()>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
{
    let ops_db = schema
        .data::<DatabaseConnection>()
        .ok_or_else(|| {
            KabiPayError::Internal("ops DatabaseConnection missing from schema data".into())
        })?;
    let cache = schema
        .data::<TenantDbCache>()
        .ok_or_else(|| KabiPayError::Internal("TenantDbCache missing from schema data".into()))?;
    let fallback = schema
        .data::<TenantDbConfig>()
        .ok_or_else(|| {
            KabiPayError::Internal("TenantDbConfig missing from schema data".into())
        })?;
    let db = resolve_tenant_db(tenant_id, ops_db, cache, fallback).await?;
    let row = user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .one(&db)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;

    if account_allows_client_access(row.map(|found| (found.is_active, found.is_deleted))) {
        Ok(())
    } else {
        Err(KabiPayError::Unauthorised)
    }
}

fn account_allows_client_access(account_state: Option<(bool, bool)>) -> bool {
    matches!(account_state, Some((true, false)))
}

#[cfg(test)]
mod active_client_tests {
    use super::account_allows_client_access;

    #[test]
    fn only_active_non_deleted_users_keep_client_access() {
        assert!(account_allows_client_access(Some((true, false))));
        assert!(!account_allows_client_access(Some((false, false))));
        assert!(!account_allows_client_access(Some((true, true))));
        assert!(!account_allows_client_access(Some((false, true))));
        assert!(!account_allows_client_access(None));
    }
}

/// Playground HTML for `GET /graphql` on a tenant subgraph.
pub async fn graphql_playground() -> impl IntoResponse {
    axum::response::Html(async_graphql::http::playground_source(
        async_graphql::http::GraphQLPlaygroundConfig::new("/graphql"),
    ))
}

enum RequestIdentity {
    Client {
        tenant_id: Uuid,
        claims: Option<ClientClaims>,
    },
    Operator(OperatorClaims),
    None,
}

/// Resolve who is calling the subgraph.
///
/// 1. `Authorization: Bearer <jwt>` — `kabipay-client` → tenant + optional claims;
///    `kabipay-ops` → operator context (no tenant id).
/// 2. No bearer: when `KABIPAY_REQUIRE_AUTH=1`, anonymous. Otherwise `x-tenant-id`
///    sets a dev tenant id without JWT claims.
fn extract_request_identity(headers: &HeaderMap) -> Result<RequestIdentity, (StatusCode, String)> {
    if let Some(auth) = headers.get(header::AUTHORIZATION) {
        let auth_str = auth.to_str().map_err(|_| {
            (
                StatusCode::BAD_REQUEST,
                "authorization header is not valid ASCII".to_string(),
            )
        })?;
        let token = extract_bearer(auth_str).map_err(|_| {
            (
                StatusCode::UNAUTHORIZED,
                "authorization header must be `Bearer <token>`".to_string(),
            )
        })?;
        let secret = jwt_secret_from_env();
        if let Ok(claims) = decode_client_jwt(token, &secret) {
            return Ok(RequestIdentity::Client {
                tenant_id: claims.tenant_id,
                claims: Some(claims),
            });
        }
        if let Ok(op) = decode_operator_jwt(token, &secret) {
            return Ok(RequestIdentity::Operator(op));
        }
        return Err((
            StatusCode::UNAUTHORIZED,
            "invalid bearer token (not a valid client or operator JWT)".to_string(),
        ));
    }

    if std::env::var("KABIPAY_REQUIRE_AUTH").as_deref() == Ok("1") {
        return Ok(RequestIdentity::None);
    }

    let Some(raw) = headers.get(TENANT_HEADER) else {
        return Ok(RequestIdentity::None);
    };
    let raw = raw.to_str().map_err(|_| {
        (
            StatusCode::BAD_REQUEST,
            format!("{TENANT_HEADER} is not valid ASCII"),
        )
    })?;
    let tenant_id = Uuid::parse_str(raw.trim()).map_err(|e| {
        (
            StatusCode::BAD_REQUEST,
            format!("{TENANT_HEADER} is not a UUID: {e}"),
        )
    })?;
    Ok(RequestIdentity::Client {
        tenant_id,
        claims: None,
    })
}
