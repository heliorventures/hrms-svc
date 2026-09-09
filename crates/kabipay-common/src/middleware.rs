//! Axum middleware for authentication and module gating.
//!
//! Authentication layers:
//!   1. `operator_auth` — validates the `kabipay-ops` JWT and injects `OperatorContext`.
//!   2. `client_auth`   — validates the `kabipay-client` JWT and injects `ClientContext`.
//! Module enforcement is implemented by `entitlement_graphql::ModuleEntitlement`
//! and `entitlements::require_current_module` on the actual GraphQL/HTTP paths.
//!
//! Service boundaries enforce modules independently of gateway routing.

use crate::context::{ClientClaims, ClientContext, OperatorClaims, OperatorContext, ScopeType};
use crate::error::KabiPayError;
use crate::jwt::{decode_client_jwt, decode_operator_jwt, extract_bearer};
use axum::{
    extract::{Request, State},
    http::{header, HeaderMap},
    middleware::Next,
    response::Response,
};
use std::collections::HashMap;
use std::sync::Arc;

/// Config passed to auth middleware. Built from env once at service startup.
#[derive(Debug, Clone)]
pub struct AuthConfig {
    pub operator_jwt_secret: Arc<Vec<u8>>,
    pub client_jwt_secret: Arc<Vec<u8>>,
}

/// Validate operator JWT and inject `OperatorContext` into request extensions.
/// Use on `/graphql/ops` and operator-only routes.
pub async fn operator_auth(
    State(cfg): State<AuthConfig>,
    mut req: Request,
    next: Next,
) -> Result<Response, KabiPayError> {
    let claims = extract_operator_claims(&cfg, req.headers())?;
    let ctx = OperatorContext {
        operator_user_id: claims.sub,
        roles: claims.roles,
        tenant_access: claims.tenant_access,
    };
    req.extensions_mut().insert(ctx);
    Ok(next.run(req).await)
}

/// Validate client JWT and inject `ClientContext` into request extensions.
/// Use on `/graphql/client` and any tenant-facing route.
pub async fn client_auth(
    State(cfg): State<AuthConfig>,
    mut req: Request,
    next: Next,
) -> Result<Response, KabiPayError> {
    let claims = extract_client_claims(&cfg, req.headers())?;
    let ctx = ClientContext {
        user_id: claims.sub,
        tenant_id: claims.tenant_id,
        employee_id: claims.employee_id,
        roles: claims.roles,
        permissions: claims.permissions,
        // TODO: resolve scopes from PERMISSION_SCOPE table. For now default all to Self.
        scopes: HashMap::<String, ScopeType>::new(),
    };
    req.extensions_mut().insert(ctx);
    Ok(next.run(req).await)
}

fn extract_operator_claims(
    cfg: &AuthConfig,
    headers: &HeaderMap,
) -> Result<OperatorClaims, KabiPayError> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let token = extract_bearer(auth)?;
    decode_operator_jwt(token, cfg.operator_jwt_secret.as_ref())
}

fn extract_client_claims(
    cfg: &AuthConfig,
    headers: &HeaderMap,
) -> Result<ClientClaims, KabiPayError> {
    let auth = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let token = extract_bearer(auth)?;
    decode_client_jwt(token, cfg.client_jwt_secret.as_ref())
}
