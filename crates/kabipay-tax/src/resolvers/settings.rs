//! Shared authorization boundary for employee tax input and projection views.
use async_graphql::{Context, Result, ID};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_claims, resolve_employee_scope_filter, resolve_viewer_employee,
    },
    context::{ClientClaims, ScopeType, PERM_TAX_MANAGE, PERM_TAX_READ},
    subgraph::resolve_client_employee_id,
    KabiPayError,
};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

pub fn manager(ctx: &Context<'_>) -> Result<Uuid> {
    let claims = ctx.data::<ClientClaims>()?;
    let scope = data_scope_from_claims(Some(claims), PERM_TAX_MANAGE)
        .map_err(KabiPayError::into_graphql)?;
    if scope != ScopeType::All {
        return Err(KabiPayError::Forbidden("tax:manage requires ALL scope".into()).into_graphql());
    }
    Ok(claims.sub)
}
pub fn employee_id(value: &ID) -> Result<Uuid> {
    Uuid::parse_str(value.as_str())
        .map_err(|_| KabiPayError::Validation("invalid employee ID".into()).into_graphql())
}
pub async fn target(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant: Uuid,
    requested: Option<ID>,
) -> Result<Uuid> {
    let scope = data_scope_from_claims(ctx.data_opt::<ClientClaims>(), PERM_TAX_READ)
        .map_err(KabiPayError::into_graphql)?;
    let id = match requested {
        Some(id) => employee_id(&id)?,
        None => resolve_client_employee_id(ctx, db, tenant)
            .await
            .map_err(KabiPayError::into_graphql)?,
    };
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer_employee(ctx, db, tenant).await?
    };
    let filter = resolve_employee_scope_filter(db, tenant, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    if !filter.allows_employee(id) {
        return Err(
            KabiPayError::Forbidden("tax:read scope excludes this employee".into()).into_graphql(),
        );
    }
    crate::services::tax_settings::require_employee(db, tenant, id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    Ok(id)
}
