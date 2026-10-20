//! Current source-account eligibility for the organization control plane.
use crate::{rbac, state::AppState};
use axum::{
    extract::State,
    http::{header, HeaderMap},
    response::IntoResponse,
    Json,
};
use kabipay_common::{
    db::resolve_required_tenant_db,
    jwt::{extract_bearer},
    KabiPayError,
};
use kabipay_db_entities::{ops::tenant, tenant::d0005_auth_rbac::user};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use serde::{Deserialize, Serialize};
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct IdentityRequest {
    nonce: Uuid,
}
#[derive(Serialize)]
struct IdentityResponse {
    nonce: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    issuer: &'static str,
    display_name: String,
    permission_scopes: std::collections::HashMap<String, String>,
    assurance: &'static str,
}
pub async fn organization_identity(
    State(state): State<AppState>,
    headers: HeaderMap,
    Json(input): Json<IdentityRequest>,
) -> Result<impl IntoResponse, KabiPayError> {
    if input.nonce.is_nil() {
        return Err(KabiPayError::Validation("nonce is required".into()));
    }
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|v| v.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let claims = kabipay_common::client_jwt::verify_client_token(extract_bearer(bearer)?, &state.jwt.secret)?;
    state.tenant_cache.require_tenant(claims.tenant_id)?;
    let company = tenant::Entity::find_by_id(claims.tenant_id)
        .one(&state.ops_db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;
    if company.status != "ACTIVE" || company.is_deleted {
        return Err(KabiPayError::Unauthorised);
    }
    let db = resolve_required_tenant_db(
        claims.tenant_id,
        &state.ops_db,
        &state.tenant_cache,
        &state.tenant_fallback,
    )
    .await?;
    let account = user::Entity::find_by_id(claims.sub)
        .filter(user::Column::TenantId.eq(claims.tenant_id))
        .one(&db)
        .await?
        .ok_or(KabiPayError::Unauthorised)?;
    if !eligible(
        account.is_active,
        account.is_deleted,
        account.must_change_password,
    ) {
        return Err(KabiPayError::Unauthorised);
    }
    if !crate::account_eligibility::linked_employment_eligible(&db,claims.tenant_id,claims.sub).await? {return Err(KabiPayError::Unauthorised);}
    let authorization = rbac::load_client_authorization(&db, claims.tenant_id, claims.sub).await?;
    Ok((
        [(header::CACHE_CONTROL, "no-store")],
        Json(IdentityResponse {
            nonce: input.nonce,
            tenant_id: claims.tenant_id,
            user_id: claims.sub,
            issuer: "kabipay-client",
            display_name: account.username,
            permission_scopes: authorization.permission_scopes,
            assurance: if std::env::var("KABIPAY_CLIENT_JWT_MODE").as_deref()==Ok("rs256") {"rs256-company-bound"}else{"legacy"},
        }),
    ))
}
fn eligible(active: bool, deleted: bool, must_change_password: bool) -> bool {
    active && !deleted && !must_change_password
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn organization_identity_rejects_disabled_deleted_and_incomplete_accounts() {
        assert!(eligible(true, false, false));
        assert!(!eligible(false, false, false));
        assert!(!eligible(true, true, false));
        assert!(!eligible(true, false, true));
    }
    #[tokio::test]
    async fn source_identity_rejects_foreign_company_before_any_database_lookup() {
        use kabipay_common::db::{BoundTenantDatabase, TenantDbCache, TenantDbConfig};
        let secret = b"synthetic-organization-auth-test";
        let state = AppState {
            ops_db: sea_orm::DatabaseConnection::Disconnected,
            tenant_cache: TenantDbCache::with_binding(
                BoundTenantDatabase::from_values(
                    Some(&Uuid::from_u128(1).to_string()),
                    Some("postgres://fixture:synthetic@127.0.0.1:1/fixture"),
                    Some("tenant_fixture"),
                )
                .unwrap()
                .unwrap(),
            ),
            tenant_fallback: TenantDbConfig {
                db_host: "unused".into(),
                db_port: 1,
                db_name: "unused".into(),
                db_user: "unused".into(),
                db_password: "unused".into(),
                schema_name: "unused".into(),
            },
            jwt: crate::jwt::JwtConfig {
                secret: secret.to_vec(),
                ops_access_ttl_secs: 60,
                client_access_ttl_secs: 60,
                refresh_ttl_secs: 60,
            },
        };
        let claims=serde_json::from_value(serde_json::json!({"sub":Uuid::from_u128(3),"tenant_id":Uuid::from_u128(2),"iss":"kabipay-client","iat":chrono::Utc::now().timestamp(),"exp":chrono::Utc::now().timestamp()+60})).unwrap();
        let token = kabipay_common::jwt::encode_client_jwt(&claims, secret).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            format!("Bearer {token}").parse().unwrap(),
        );
        assert!(matches!(
            organization_identity(
                State(state),
                headers,
                Json(IdentityRequest {
                    nonce: Uuid::new_v4()
                })
            )
            .await,
            Err(KabiPayError::Forbidden(_))
        ));
    }
}
