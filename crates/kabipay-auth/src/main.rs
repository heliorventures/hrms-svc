//! kabipay-auth
//!
//! REST auth service for both planes. Exposes (plain HTTP, no JWT required):
//!
//!   POST /auth/ops/login      { email, password }          → TokenPair
//!   POST /auth/ops/mfa        { mfaToken, code }           → 501 (scaffolded)
//!   POST /auth/ops/refresh    { refresh }                  → TokenPair (rotated)
//!   POST /auth/ops/logout     { refresh }                  → 204
//!
//!   POST /auth/client/login   { email, password, tenantId }→ TokenPair
//!   POST /auth/client/mfa     { mfaToken, code }           → 501 (scaffolded)
//!   POST /auth/client/refresh { refresh }                  → TokenPair (rotated)
//!   POST /auth/client/logout  { refresh }                  → 204
//!   POST /auth/client/change-password { currentPassword, newPassword } → 204 (Bearer access)
//!
//!   POST /auth/introspect     { token }                    → { active, userId, tenantId, issuer, email, exp }
//!
//!   GET  /healthz                                          → 200 "ok"

use axum::{
    middleware::from_fn,
    routing::{get, post},
    Router,
};
use kabipay_common::{
    db::TenantDbCache,
    load_dotenv,
    subgraph::{ops_dsn_from_env, tenant_db_config_from_env},
    telemetry::init_tracing,
};
use std::net::SocketAddr;
use tower_http::cors::CorsLayer;

mod handlers;
use kabipay_auth::{organization_identity,account_eligibility,jwt,state};
mod password_tasks;
mod request_metrics;
use kabipay_auth::rbac;
mod tokens;

use state::AppState;

fn install_panic_hook() {
    let default_hook = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        default_hook(info);
        eprintln!(
            "kabipay-auth: process panicked. If you only see `exit code: 0xffffffff` from `cargo run`, try: \
             `cargo build -p kabipay-auth` then `.\\target\\debug\\kabipay-auth.exe` (or exclude the repo from real-time AV)."
        );
    }));
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_hook();
    load_dotenv();
    init_tracing("kabipay-auth");
    let plane = kabipay_auth::plane::AuthPlane::from_env()?;
    if plane.includes_client() {
        kabipay_common::client_jwt::validate_client_token_configuration(true)?;
    }

    let port: u16 = std::env::var("KABIPAY_AUTH_PORT")
        .unwrap_or_else(|_| "4001".to_string())
        .parse()
        .unwrap_or(4001);
    tracing::info!(port, "kabipay-auth resolved listen port");

    let tenant_cache = TenantDbCache::from_env()?;
    let dsn = ops_dsn_from_env();
    let lifecycle = kabipay_common::service_lifecycle::LifecycleConfig::from_env()?;
    let listener = tokio::net::TcpListener::bind(SocketAddr::from(([0, 0, 0, 0], port))).await?;
    let initialization = lifecycle.clone();
    kabipay_common::service_lifecycle::serve(listener, lifecycle, move |_| async move {
        let ops_db = kabipay_common::db::availability::connect_validated(
            &dsn,
            &tenant_cache,
            &initialization,
        )
        .await?;
        let check =
            kabipay_common::db::availability::database_check(ops_db.clone(), tenant_cache.clone());
        let app_state = AppState {
            ops_db,
            tenant_cache,
            tenant_fallback: tenant_db_config_from_env(),
            jwt: handlers::jwt_config_from_env(),
        };

        let ops_routes = Router::new()
            // Operator plane
            .route("/auth/ops/login", post(handlers::ops_login))
            .route("/auth/ops/mfa", post(handlers::ops_mfa))
            .route("/auth/ops/refresh", post(handlers::ops_refresh))
            .route("/auth/ops/logout", post(handlers::ops_logout));
        // Client plane
        let client_routes = Router::new()
            .route(
                "/auth/client/tenants/:slug",
                get(handlers::resolve_client_tenant),
            )
            .route("/auth/client/login", post(handlers::client_login))
            .route(
                "/auth/client/organization-identity",
                post(organization_identity::organization_identity),
            )
            .route(
                "/auth/internal/account-eligibility",
                post(account_eligibility::account_eligibility),
            )
            .route(
                "/auth/internal/employment-eligibility",
                post(account_eligibility::employment_eligibility),
            )
            .route("/auth/client/mfa", post(handlers::client_mfa))
            .route("/auth/client/refresh", post(handlers::client_refresh))
            .route("/auth/client/logout", post(handlers::client_logout))
            .route(
                "/auth/client/change-password",
                post(handlers::client_change_password),
            )
            // Token introspection (used by gateway + subgraph auth middleware)
            .route("/auth/introspect", post(handlers::introspect));
        let app = plane
            .routes(ops_routes, client_routes)
            .with_state(app_state)
            .layer(CorsLayer::permissive())
            .layer(from_fn(request_metrics::record_request));
        let app = if std::env::var_os("KABIPAY_SERVICE_TRUST_KEYS").is_some() {
            app.layer(axum::Extension(std::sync::Arc::new(
                kabipay_organization_security::service_verifier_from_env("kabipay-auth")?,
            )))
        } else {
            app
        };

        Ok(kabipay_common::service_lifecycle::PreparedService { router: app, check })
    })
    .await
}

/// Fallback 501 response used by stub handlers until they are implemented.
pub fn not_implemented(
    endpoint: &'static str,
) -> (axum::http::StatusCode, axum::Json<serde_json::Value>) {
    (
        axum::http::StatusCode::NOT_IMPLEMENTED,
        axum::Json(serde_json::json!({
            "error": {
                "code": "NOT_IMPLEMENTED",
                "message": format!("{endpoint} is scaffolded but not yet implemented"),
            }
        })),
    )
}
