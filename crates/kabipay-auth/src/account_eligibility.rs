//! Private current-account check; never accepts a user JWT as a service credential.
use crate::state::AppState;
use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap},
    response::IntoResponse,
    Extension, Json,
};
use kabipay_common::{db::resolve_required_tenant_db, jwt::extract_bearer, KabiPayError};
use kabipay_db_entities::{ops::tenant, tenant::d0005_auth_rbac::user};
use kabipay_organization_protocol::{
    AccountEligibilityRequest, AccountEligibilityResponse, EmploymentEligibilityRequest,
    EmploymentEligibilityResponse,
};
use kabipay_organization_security::ServiceCredentialVerifier;
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement};
use std::sync::Arc;
pub(crate) async fn linked_employment_eligible(
    db: &sea_orm::DatabaseConnection,
    tenant: uuid::Uuid,
    user: uuid::Uuid,
) -> Result<bool, KabiPayError> {
    let linked=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT count(*) AS count,count(*) FILTER(WHERE NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE')) AS eligible FROM employee WHERE tenant_id=$1 AND user_id=$2",vec![tenant.into(),user.into()])).await?.ok_or(KabiPayError::Unauthorised)?;
    let count: i64 = linked.try_get("", "count")?;
    let active: i64 = linked.try_get("", "eligible")?;
    Ok(count == 0 || count == 1 && active == 1)
}
pub async fn account_eligibility(
    State(state): State<AppState>,
    proof: Option<Extension<Arc<ServiceCredentialVerifier>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, KabiPayError> {
    if std::env::var("KABIPAY_CLIENT_JWT_MODE").as_deref()!=Ok("rs256"){return Err(KabiPayError::Unauthorised)}
    let verifier = proof.ok_or(KabiPayError::Unauthorised)?;
    let target = state.tenant_cache.requested_tenant(&headers)?;
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let verified = verifier
        .verify(
            extract_bearer(bearer)?,
            target,
            "/auth/internal/account-eligibility",
            &body,
        )
        .map_err(|_| KabiPayError::Unauthorised)?;
    if verified.claims().service != "kabipay-organization" {
        return Err(KabiPayError::Unauthorised);
    }
    let input: AccountEligibilityRequest = serde_json::from_slice(&body)
        .map_err(|_| KabiPayError::Validation("invalid eligibility request".into()))?;
    if input.user_id.is_nil() || input.nonce.is_nil() {
        return Err(KabiPayError::Unauthorised);
    }
    let company = tenant::Entity::find_by_id(target)
        .one(&state.ops_db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;
    if company.status != "ACTIVE" || company.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }
    let db = resolve_required_tenant_db(
        target,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await?;
    let account_eligible = user::Entity::find_by_id(input.user_id)
        .filter(user::Column::TenantId.eq(target))
        .one(&db)
        .await?
        .is_some_and(|u| u.is_active && !u.is_deleted && !u.must_change_password);
    let eligible =
        account_eligible && linked_employment_eligible(&db, target, input.user_id).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(AccountEligibilityResponse {
            user_id: input.user_id,
            tenant_id: target,
            nonce: input.nonce,
            eligible,
        }),
    ))
}
pub async fn employment_eligibility(
    State(state): State<AppState>,
    proof: Option<Extension<Arc<ServiceCredentialVerifier>>>,
    headers: HeaderMap,
    body: Bytes,
) -> Result<impl IntoResponse, KabiPayError> {
    if std::env::var("KABIPAY_CLIENT_JWT_MODE").as_deref()!=Ok("rs256"){return Err(KabiPayError::Unauthorised)}
    let verifier = proof.ok_or(KabiPayError::Unauthorised)?;
    let target = state.tenant_cache.requested_tenant(&headers)?;
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let verified = verifier
        .verify(
            extract_bearer(bearer)?,
            target,
            "/auth/internal/employment-eligibility",
            &body,
        )
        .map_err(|_| KabiPayError::Unauthorised)?;
    if verified.claims().service != "kabipay-organization" {
        return Err(KabiPayError::Unauthorised);
    }
    let input: EmploymentEligibilityRequest = serde_json::from_slice(&body)?;
    if input.employee_id.is_nil() || input.nonce.is_nil() {
        return Err(KabiPayError::Unauthorised);
    }
    let company = tenant::Entity::find_by_id(target)
        .one(&state.ops_db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;
    if company.status != "ACTIVE" || company.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }
    let db = resolve_required_tenant_db(
        target,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await?;
    let eligible=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,"SELECT id FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE') AND user_id IS NOT DISTINCT FROM $3",vec![target.into(),input.employee_id.into(),input.expected_user_id.into()])).await?.is_some();
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(EmploymentEligibilityResponse {
            employee_id: input.employee_id,
            tenant_id: target,
            nonce: input.nonce,
            eligible,
        }),
    ))
}
