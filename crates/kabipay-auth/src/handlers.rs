//! REST handlers for `kabipay-auth`.
//!
//! OPS LOGIN:
//!   1. Find `operator_user` by email in `kabipay_ops`.
//!      Reject on `is_active=false`, `is_deleted=true`, or missing row.
//!   2. Verify password via argon2id.
//!   3. Issue operator JWT (iss=kabipay-ops) + opaque refresh. Persist a
//!      SHA-256 digest of the refresh token in `operator_session`.
//!
//! CLIENT LOGIN:
//!   1. Require `tenant_id` in the body (subdomain-based resolution is
//!      deferred to the tenant service).
//!   2. Resolve the tenant pool via `kabipay_common::db::resolve_required_tenant_db`,
//!      then look up `user` by email.
//!   3. Issue client JWT (iss=kabipay-client, tenant_id claim) + refresh.
//!      Persist refresh digest in `user_session`.
//!
//! REFRESH / LOGOUT:
//!   Look up the session by `token_hash`, rotate it (refresh) or delete it
//!   (logout). Any session lookup miss returns 401.
//!
//! MFA endpoints are still scaffolded 501 — we wire them once the MFA
//! enrolment flow is designed.

use axum::{
    extract::{Path, State},
    http::HeaderMap,
    http::StatusCode,
    Json,
};
use chrono::{Duration, Utc};
use kabipay_common::{
    db::resolve_required_tenant_db,
    error::{KabiPayError, KabiPayResult},
    jwt::{decode_client_jwt, extract_bearer},
};
use kabipay_db_entities::{
    ops::{operator_session, operator_user, tenant},
    tenant::d0005_auth_rbac::{user, user_session},
    tenant::d0007_employee_core::employee,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
};
use serde::{Deserialize, Serialize};
use std::time::Instant;
use uuid::Uuid;

use crate::{
    jwt::JwtConfig, not_implemented, password_tasks, rbac, request_metrics, state::AppState, tokens,
};

// ---------------------------------------------------------------------------
// DTOs
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize)]
pub struct OpsLoginInput {
    pub email: String,
    pub password: String,
}

#[derive(Debug, Deserialize)]
pub struct ClientLoginInput {
    pub username: String,
    pub password: String,
    #[serde(rename = "tenantId")]
    pub tenant_id: Option<Uuid>,
    pub subdomain: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct MfaInput {
    #[serde(rename = "mfaToken")]
    pub mfa_token: String,
    pub code: String,
}

#[derive(Debug, Deserialize)]
pub struct RefreshInput {
    pub refresh: String,
}

#[derive(Debug, Deserialize)]
pub struct ClientChangePasswordInput {
    #[serde(rename = "currentPassword")]
    pub current_password: String,
    #[serde(rename = "newPassword")]
    pub new_password: String,
}

#[derive(Debug, Serialize)]
pub struct TokenPair {
    pub access: String,
    pub refresh: String,
    #[serde(rename = "tokenType")]
    pub token_type: &'static str,
    #[serde(rename = "expiresIn")]
    pub expires_in: i64,
    #[serde(rename = "tenantId", skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub username: Option<String>,
    pub email: String,
    #[serde(rename = "userId")]
    pub user_id: Uuid,
    #[serde(rename = "mustChangePassword")]
    pub must_change_password: bool,
}

#[derive(Debug, Serialize)]
pub struct ResolvedTenant {
    pub id: Uuid,
    pub name: String,
    pub status: String,
    pub subdomain: String,
    #[serde(rename = "logoUrl", skip_serializing_if = "Option::is_none")]
    pub logo_url: Option<String>,
    #[serde(rename = "primaryColor", skip_serializing_if = "Option::is_none")]
    pub primary_color: Option<String>,
}

// ---------------------------------------------------------------------------
// Ops plane
// ---------------------------------------------------------------------------

pub async fn ops_login(
    State(state): State<AppState>,
    Json(body): Json<OpsLoginInput>,
) -> Result<Json<TokenPair>, KabiPayError> {
    let row = operator_user::Entity::find()
        .filter(operator_user::Column::Email.eq(body.email.to_lowercase()))
        .one(&state.ops_db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;

    if !row.is_active || row.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }
    if !password_tasks::verify(body.password, row.password_hash.clone()).await? {
        return Err(KabiPayError::Unauthorised);
    }

    touch_operator_last_login(&state.ops_db, row.id).await?;

    let pair = issue_ops_tokens(&state, row.id, &row.email).await?;
    Ok(Json(pair))
}

pub async fn ops_refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshInput>,
) -> Result<Json<TokenPair>, KabiPayError> {
    let hash = tokens::hash_refresh(&body.refresh);
    let session = operator_session::Entity::find()
        .filter(operator_session::Column::TokenHash.eq(hash.clone()))
        .one(&state.ops_db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;

    if session.expires_at < Utc::now() {
        let _ = operator_session::Entity::delete_by_id(session.id)
            .exec(&state.ops_db)
            .await;
        return Err(KabiPayError::Unauthorised);
    }

    let user_row = operator_user::Entity::find_by_id(session.operator_user_id)
        .one(&state.ops_db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;
    if !user_row.is_active || user_row.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }

    let consumed = operator_session::Entity::delete_by_id(session.id)
        .exec(&state.ops_db)
        .await?;
    if consumed.rows_affected != 1 {
        return Err(KabiPayError::Unauthorised);
    }

    let pair = issue_ops_tokens(&state, user_row.id, &user_row.email).await?;
    Ok(Json(pair))
}

pub async fn ops_logout(
    State(state): State<AppState>,
    Json(body): Json<RefreshInput>,
) -> StatusCode {
    let hash = tokens::hash_refresh(&body.refresh);
    let _ = operator_session::Entity::delete_many()
        .filter(operator_session::Column::TokenHash.eq(hash))
        .exec(&state.ops_db)
        .await;
    StatusCode::NO_CONTENT
}

pub async fn ops_mfa(Json(_body): Json<MfaInput>) -> impl axum::response::IntoResponse {
    not_implemented("POST /auth/ops/mfa")
}

// ---------------------------------------------------------------------------
// Client plane
// ---------------------------------------------------------------------------

fn normalize_tenant_slug(raw: &str) -> KabiPayResult<String> {
    let slug = raw.trim().to_lowercase();
    let valid = slug.len() >= 2
        && slug.len() <= 63
        && slug
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !slug.starts_with('-')
        && !slug.ends_with('-');
    if !valid {
        return Err(KabiPayError::Validation("tenant slug is invalid".into()));
    }
    Ok(slug)
}

pub async fn resolve_client_tenant(
    State(state): State<AppState>,
    Path(slug): Path<String>,
) -> Result<Json<ResolvedTenant>, KabiPayError> {
    let row = find_tenant_by_slug(&state.ops_db, &slug).await?;

    Ok(Json(ResolvedTenant {
        id: row.id,
        name: row.name,
        status: row.status,
        subdomain: row
            .subdomain
            .unwrap_or_else(|| normalize_tenant_slug(&slug).unwrap_or(slug)),
        logo_url: row.logo_url,
        primary_color: row.primary_color,
    }))
}

pub async fn client_login(
    State(state): State<AppState>,
    Json(body): Json<ClientLoginInput>,
) -> Result<Json<TokenPair>, KabiPayError> {
    let tenant_id = match body.tenant_id {
        Some(id) => id,
        None => {
            let slug = body.subdomain.as_deref().ok_or_else(|| {
                KabiPayError::Validation("tenantId or subdomain is required".into())
            })?;
            let started = Instant::now();
            let row = find_tenant_by_slug(&state.ops_db, slug).await;
            request_metrics::record_phase("tenant_lookup", None, started);
            row?.id
        }
    };

    let started = Instant::now();
    let tenant_conn = resolve_required_tenant_db(
        tenant_id,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await;
    request_metrics::record_phase("tenant_pool", Some(tenant_id), started);
    let tenant_conn = tenant_conn?;

    let login_username = body.username.trim().to_lowercase();
    if login_username.is_empty() {
        return Err(KabiPayError::Validation("username is required".into()));
    }

    let started = Instant::now();
    let row = user::Entity::find()
        .filter(user::Column::Username.eq(login_username))
        .filter(user::Column::TenantId.eq(tenant_id))
        .one(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error));
    request_metrics::record_phase("user_lookup", Some(tenant_id), started);
    let row = row?.ok_or(KabiPayError::Unauthorised)?;

    if !row.is_active || row.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }
    let started = Instant::now();
    let password_matches = password_tasks::verify(body.password, row.password_hash.clone()).await;
    request_metrics::record_phase("password_verify", Some(tenant_id), started);
    if !password_matches? {
        return Err(KabiPayError::Unauthorised);
    }

    let last_login = touch_user_last_login(&tenant_conn, tenant_id, row.id);
    let tokens = issue_client_tokens(
        &state,
        &tenant_conn,
        row.id,
        tenant_id,
        &row.username,
        row.email.as_deref(),
        row.must_change_password,
    );
    let (_, pair) = tokio::join!(last_login, tokens);
    let pair = pair?;
    Ok(Json(pair))
}

pub async fn client_refresh(
    State(state): State<AppState>,
    Json(body): Json<RefreshInput>,
) -> Result<Json<TokenPair>, KabiPayError> {
    let hash = tokens::hash_refresh(&body.refresh);

    // Client refresh tokens are issued with a `<tenant_uuid>.<opaque>` prefix
    // so this service can route the lookup without a reverse index. The DB
    // still only ever sees the SHA-256 of the full string.
    let tenant_id = peek_tenant_from_refresh(&body.refresh)
        .ok_or_else(|| KabiPayError::Validation("refresh token is not tenant-scoped".into()))?;

    let started = Instant::now();
    let tenant_conn = resolve_required_tenant_db(
        tenant_id,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await;
    request_metrics::record_phase("tenant_pool", Some(tenant_id), started);
    let tenant_conn = tenant_conn?;

    let started = Instant::now();
    let session = user_session::Entity::find()
        .filter(user_session::Column::TokenHash.eq(hash.clone()))
        .one(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error));
    request_metrics::record_phase("session_lookup", Some(tenant_id), started);
    let session = session?.ok_or(KabiPayError::Unauthorised)?;

    if session.expires_at < Utc::now() {
        let started = Instant::now();
        let _ = user_session::Entity::delete_by_id(session.id)
            .exec(&tenant_conn)
            .await
            .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
        request_metrics::record_phase("session_persist", Some(tenant_id), started);
        return Err(KabiPayError::Unauthorised);
    }

    let started = Instant::now();
    let user_row = user::Entity::find_by_id(session.user_id)
        .one(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error));
    request_metrics::record_phase("user_lookup", Some(tenant_id), started);
    let user_row = user_row?.ok_or(KabiPayError::Unauthorised)?;
    if !user_row.is_active || user_row.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }

    let started = Instant::now();
    let consumed = user_session::Entity::delete_by_id(session.id)
        .exec(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    request_metrics::record_phase("session_persist", Some(tenant_id), started);
    if consumed.rows_affected != 1 {
        return Err(KabiPayError::Unauthorised);
    }

    let pair = issue_client_tokens(
        &state,
        &tenant_conn,
        user_row.id,
        tenant_id,
        &user_row.username,
        user_row.email.as_deref(),
        user_row.must_change_password,
    )
    .await?;
    Ok(Json(pair))
}

pub async fn client_logout(
    State(state): State<AppState>,
    Json(body): Json<RefreshInput>,
) -> StatusCode {
    let Some(tenant_id) = peek_tenant_from_refresh(&body.refresh) else {
        return StatusCode::NO_CONTENT;
    };
    let started = Instant::now();
    let tenant_conn = resolve_required_tenant_db(
        tenant_id,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await;
    request_metrics::record_phase("tenant_pool", Some(tenant_id), started);
    let Ok(tenant_conn) = tenant_conn else {
        return StatusCode::NO_CONTENT;
    };
    let hash = tokens::hash_refresh(&body.refresh);
    let _ = user_session::Entity::delete_many()
        .filter(user_session::Column::TokenHash.eq(hash))
        .exec(&tenant_conn)
        .await;
    StatusCode::NO_CONTENT
}

pub async fn client_mfa(Json(_body): Json<MfaInput>) -> impl axum::response::IntoResponse {
    not_implemented("POST /auth/client/mfa")
}

/// Change password for the signed-in client user. Requires `Authorization: Bearer <access>`.
/// On success: updates hash, revokes **all** refresh sessions for the user (force re-login).
pub async fn client_change_password(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(body): Json<ClientChangePasswordInput>,
) -> Result<StatusCode, KabiPayError> {
    const MIN_LEN: usize = 8;
    let auth_header = headers
        .get(axum::http::header::AUTHORIZATION)
        .and_then(|h| h.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let token = extract_bearer(auth_header)?;
    let claims = decode_client_jwt(token, &state.jwt.secret)?;
    let tenant_id = claims.tenant_id;
    let user_id = claims.sub;

    let new_password = body.new_password;
    let current_password = body.current_password;
    let new_pw = new_password.trim();
    if new_pw.len() < MIN_LEN {
        return Err(KabiPayError::Validation(format!(
            "newPassword must be at least {MIN_LEN} characters"
        )));
    }
    if new_pw == current_password {
        return Err(KabiPayError::BusinessRule {
            code: "PASSWORD_REUSE_NOT_ALLOWED",
            message: "New password must be different from the current password.".into(),
        });
    }

    let started = Instant::now();
    let tenant_conn = resolve_required_tenant_db(
        tenant_id,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await;
    request_metrics::record_phase("tenant_pool", Some(tenant_id), started);
    let tenant_conn = tenant_conn?;

    let started = Instant::now();
    let user_row = user::Entity::find_by_id(user_id)
        .one(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error));
    request_metrics::record_phase("user_lookup", Some(tenant_id), started);
    let user_row = user_row?.ok_or(KabiPayError::Unauthorised)?;
    if user_row.tenant_id != tenant_id || !user_row.is_active || user_row.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }
    let started = Instant::now();
    let password_matches =
        password_tasks::verify(current_password, user_row.password_hash.clone()).await;
    request_metrics::record_phase("password_verify", Some(tenant_id), started);
    if !password_matches? {
        return Err(KabiPayError::BusinessRule {
            code: "CURRENT_PASSWORD_INCORRECT",
            message: "The current password is incorrect.".into(),
        });
    }

    let started = Instant::now();
    let new_hash = password_tasks::hash(new_pw.to_owned()).await;
    request_metrics::record_phase("password_hash", Some(tenant_id), started);
    let new_hash = new_hash?;
    let mut active: user::ActiveModel = user_row.into();
    active.password_hash = ActiveValue::Set(new_hash);
    active.must_change_password = ActiveValue::Set(false);
    active.updated_at = ActiveValue::Set(Utc::now());
    let started = Instant::now();
    active
        .update(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    request_metrics::record_phase("session_persist", Some(tenant_id), started);

    let started = Instant::now();
    user_session::Entity::delete_many()
        .filter(user_session::Column::UserId.eq(user_id))
        .exec(&tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    request_metrics::record_phase("session_persist", Some(tenant_id), started);

    Ok(StatusCode::NO_CONTENT)
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

/// Client refresh tokens are issued with a `<tenant_uuid>.<opaque>` prefix
/// so a stateless service can route the lookup. The full string is also
/// what gets SHA-256 hashed for storage — so the DB still only sees the
/// digest and the tenant id is never revealed in storage.
fn peek_tenant_from_refresh(refresh: &str) -> Option<Uuid> {
    let (head, _) = refresh.split_once('.')?;
    Uuid::parse_str(head).ok()
}

async fn find_tenant_by_slug(
    db: &DatabaseConnection,
    raw_slug: &str,
) -> KabiPayResult<tenant::Model> {
    let slug = normalize_tenant_slug(raw_slug)?;
    tenant::Entity::find()
        .filter(tenant::Column::IsDeleted.eq(false))
        .filter(tenant::Column::Subdomain.eq(Some(slug.clone())))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::TenantNotFound(slug))
}

async fn issue_ops_tokens(
    state: &AppState,
    user_id: Uuid,
    email: &str,
) -> KabiPayResult<TokenPair> {
    let access = state.jwt.issue_ops_access(user_id, email)?;
    let (raw, hash) = tokens::generate_refresh();
    let expires_at = Utc::now() + Duration::seconds(state.jwt.refresh_ttl_secs);
    let session = operator_session::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        operator_user_id: ActiveValue::Set(user_id),
        token_hash: ActiveValue::Set(hash),
        ip_address: ActiveValue::Set(None),
        user_agent: ActiveValue::Set(None),
        created_at: ActiveValue::Set(Utc::now()),
        expires_at: ActiveValue::Set(expires_at),
    };
    session.insert(&state.ops_db).await?;

    Ok(TokenPair {
        access,
        refresh: raw,
        token_type: "Bearer",
        expires_in: state.jwt.ops_access_ttl_secs,
        tenant_id: None,
        username: None,
        email: email.into(),
        user_id,
        must_change_password: false,
    })
}

async fn issue_client_tokens(
    state: &AppState,
    tenant_conn: &DatabaseConnection,
    user_id: Uuid,
    tenant_id: Uuid,
    username: &str,
    email: Option<&str>,
    must_change_password: bool,
) -> KabiPayResult<TokenPair> {
    let employee_lookup = async {
        let started = Instant::now();
        let result = employee::Entity::find()
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::UserId.eq(user_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(tenant_conn)
            .await
            .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))
            .map(|row| row.map(|employee| employee.id));
        request_metrics::record_phase("employee_lookup", Some(tenant_id), started);
        result
    };
    let authorization_lookup = async {
        let started = Instant::now();
        let result = rbac::load_client_authorization(tenant_conn, tenant_id, user_id).await;
        request_metrics::record_phase("authorization", Some(tenant_id), started);
        result
    };
    let (employee_id, authorization) = tokio::try_join!(employee_lookup, authorization_lookup)?;
    let display_email = email.unwrap_or(username);
    let access = state.jwt.issue_client_access(
        user_id,
        tenant_id,
        display_email,
        employee_id,
        must_change_password,
        authorization.roles,
        authorization.permissions,
        authorization.resource_scopes,
    )?;
    // Prefix refresh with tenant id so `client_refresh` / `client_logout`
    // can look up the correct tenant schema without keeping a separate
    // reverse index. Stored hash is of the full prefixed string.
    let (opaque, _) = tokens::generate_refresh();
    let raw = format!("{tenant_id}.{opaque}");
    let hash = tokens::hash_refresh(&raw);
    let expires_at = Utc::now() + Duration::seconds(state.jwt.refresh_ttl_secs);
    let session = user_session::ActiveModel {
        id: ActiveValue::Set(Uuid::new_v4()),
        user_id: ActiveValue::Set(user_id),
        token_hash: ActiveValue::Set(hash),
        ip_address: ActiveValue::Set(None),
        user_agent: ActiveValue::Set(None),
        created_at: ActiveValue::Set(Utc::now()),
        expires_at: ActiveValue::Set(expires_at),
    };
    let started = Instant::now();
    session
        .insert(tenant_conn)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    request_metrics::record_phase("session_persist", Some(tenant_id), started);

    Ok(TokenPair {
        access,
        refresh: raw,
        token_type: "Bearer",
        expires_in: state.jwt.client_access_ttl_secs,
        tenant_id: Some(tenant_id),
        username: Some(username.into()),
        email: display_email.into(),
        user_id,
        must_change_password,
    })
}

async fn touch_operator_last_login(db: &DatabaseConnection, id: Uuid) -> KabiPayResult<()> {
    let am = operator_user::ActiveModel {
        id: ActiveValue::Unchanged(id),
        last_login_at: ActiveValue::Set(Some(Utc::now())),
        ..Default::default()
    };
    let _ = am.update(db).await;
    Ok(())
}

async fn touch_user_last_login(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> KabiPayResult<()> {
    let am = user::ActiveModel {
        id: ActiveValue::Unchanged(id),
        last_login_at: ActiveValue::Set(Some(Utc::now())),
        ..Default::default()
    };
    am.update(db)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    Ok(())
}

/// Introspection endpoint used by the gateway + subgraph middleware to
/// validate an access token without re-deriving the JWT secret everywhere.
#[derive(Debug, Serialize)]
pub struct IntrospectOutput {
    pub active: bool,
    #[serde(rename = "userId", skip_serializing_if = "Option::is_none")]
    pub user_id: Option<Uuid>,
    #[serde(rename = "tenantId", skip_serializing_if = "Option::is_none")]
    pub tenant_id: Option<Uuid>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub issuer: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub email: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub exp: Option<i64>,
}

#[derive(Debug, Deserialize)]
pub struct IntrospectInput {
    pub token: String,
}

pub async fn introspect(
    State(state): State<AppState>,
    Json(body): Json<IntrospectInput>,
) -> Json<IntrospectOutput> {
    match state.jwt.decode_any(&body.token) {
        Ok(decoded) => Json(IntrospectOutput {
            active: true,
            user_id: Some(decoded.subject),
            tenant_id: decoded.tenant_id,
            issuer: Some(decoded.issuer),
            email: Some(decoded.email),
            exp: Some(decoded.exp),
        }),
        Err(_) => Json(IntrospectOutput {
            active: false,
            user_id: None,
            tenant_id: None,
            issuer: None,
            email: None,
            exp: None,
        }),
    }
}

/// Expose [`JwtConfig`] so tests / downstream crates can issue helpers.
pub fn jwt_config_from_env() -> JwtConfig {
    JwtConfig::from_env()
}
