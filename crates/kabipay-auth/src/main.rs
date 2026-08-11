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
    db::{connect_ops_db, TenantDbCache},
    load_dotenv,
    subgraph::{ops_dsn_from_env, tenant_db_config_from_env},
    telemetry::init_tracing,
};
use std::net::SocketAddr;
use tokio::signal;
use tower_http::cors::CorsLayer;

mod handlers;
mod jwt;
mod password_tasks;
mod request_metrics;
mod rbac;
mod state;
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

async fn shutdown_signal() {
    if let Err(e) = signal::ctrl_c().await {
        tracing::warn!(error = %e, "kabipay-auth failed to listen for Ctrl+C");
    } else {
        tracing::info!("kabipay-auth Ctrl+C received, shutting down");
    }
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    install_panic_hook();
    load_dotenv();
    init_tracing("kabipay-auth");

    let port: u16 = std::env::var("KABIPAY_AUTH_PORT")
        .unwrap_or_else(|_| "4001".to_string())
        .parse()
        .unwrap_or(4001);
    tracing::info!(port, "kabipay-auth resolved listen port");

    let dsn = ops_dsn_from_env();
    tracing::info!("kabipay-auth connecting to ops database (DSN omitted)");
    let ops_db = connect_ops_db(&dsn)
        .await
        .map_err(|e| anyhow::anyhow!("kabipay-auth: failed to connect to ops DB: {e}"))?;
    tracing::info!("kabipay-auth ops database pool ready");

    let app_state = AppState {
        ops_db,
        tenant_cache: TenantDbCache::new(),
        tenant_fallback: tenant_db_config_from_env(),
        jwt: handlers::jwt_config_from_env(),
    };

    let app = Router::new()
        .route("/healthz", get(|| async { "ok" }))
        // Operator plane
        .route("/auth/ops/login", post(handlers::ops_login))
        .route("/auth/ops/mfa", post(handlers::ops_mfa))
        .route("/auth/ops/refresh", post(handlers::ops_refresh))
        .route("/auth/ops/logout", post(handlers::ops_logout))
        // Client plane
        .route(
            "/auth/client/tenants/:slug",
            get(handlers::resolve_client_tenant),
        )
        .route("/auth/client/login", post(handlers::client_login))
        .route("/auth/client/mfa", post(handlers::client_mfa))
        .route("/auth/client/refresh", post(handlers::client_refresh))
        .route("/auth/client/logout", post(handlers::client_logout))
        .route(
            "/auth/client/change-password",
            post(handlers::client_change_password),
        )
        // Token introspection (used by gateway + subgraph auth middleware)
        .route("/auth/introspect", post(handlers::introspect))
        .with_state(app_state)
        .layer(CorsLayer::permissive())
        .layer(from_fn(request_metrics::record_request));

    let addr = SocketAddr::from(([0, 0, 0, 0], port));
    tracing::info!(%addr, "kabipay-auth binding TCP listener");

    let listener = tokio::net::TcpListener::bind(addr).await?;
    tracing::info!(%addr, "kabipay-auth listening, ready for connections");
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    tracing::info!("kabipay-auth server stopped");
    Ok(())
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
