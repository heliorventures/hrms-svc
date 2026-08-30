//! Shared M3 data-scope + viewer helpers (employee list / document access).

use async_graphql::Context;
use kabipay_common::client_data_scope::{data_scope_from_context, resolve_employee_scope_filter};
use kabipay_common::context::{
    ScopeType, PERM_EMPLOYEE_MANAGE, PERM_EMPLOYEE_READ, PERM_EMPLOYEE_SELF, PERM_ROLE_MANAGE,
};
use kabipay_common::KabiPayError;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

pub use kabipay_common::client_data_scope::resolve_viewer_employee;

/// Exact `employee:read` scope. Missing, malformed, or sibling scopes fail closed.
pub fn data_scope_employee(ctx: &Context<'_>) -> async_graphql::Result<ScopeType> {
    data_scope_from_context(ctx, PERM_EMPLOYEE_READ)
}

/// Exact `employee:self` scope for JWT-bound profile and record reads.
pub fn data_scope_employee_self(ctx: &Context<'_>) -> async_graphql::Result<ScopeType> {
    data_scope_from_context(ctx, PERM_EMPLOYEE_SELF)
}

/// Require an exact permission and an explicit valid scope.
pub fn require_exact_permission_scope(
    ctx: &Context<'_>,
    permission: &str,
) -> async_graphql::Result<ScopeType> {
    data_scope_from_context(ctx, permission)
}

/// Require an exact tenant-wide permission. Management/catalogue reads never accept bounded scope.
pub fn require_exact_all_scope(
    ctx: &Context<'_>,
    permission: &str,
) -> async_graphql::Result<()> {
    let scope = require_exact_permission_scope(ctx, permission)?;
    if scope != ScopeType::All {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires ALL scope"
        ))
        .into_graphql());
    }
    Ok(())
}

/// Require at least one listed exact permission with its own explicit valid scope.
pub fn require_any_exact_scope<'a>(
    ctx: &Context<'_>,
    permissions: &'a [&'a str],
) -> async_graphql::Result<(&'a str, ScopeType)> {
    let claims = kabipay_common::subgraph::require_client_claims(ctx)?;
    let mut invalid_grant = None;
    for permission in permissions {
        if claims.has_any_permission(&[permission]) {
            if let Some(scope) = claims.scope_for_permission(permission) {
                return Ok((permission, scope));
            }
            invalid_grant.get_or_insert(*permission);
        }
    }
    if let Some(permission) = invalid_grant {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires an explicit valid scope"
        ))
        .into_graphql());
    }
    Err(KabiPayError::Forbidden(format!(
        "{} permission required",
        permissions.join(" or ")
    ))
    .into_graphql())
}

/// `employee(id)`-style visibility: target employee row must be in caller’s `employee` data scope.
pub async fn assert_employee_in_data_scope(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_emp_id: Uuid,
) -> async_graphql::Result<()> {
    let scope = data_scope_employee(ctx)?;
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer_employee(ctx, db, tenant_id).await?
    };
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    if !filter.allows_employee(target_emp_id) {
        return Err(KabiPayError::Forbidden(
            "not allowed to access this employee for documents".into(),
        )
        .into_graphql());
    }
    Ok(())
}

/// Tenant RBAC administration requires the exact `role:manage` permission.
pub fn require_tenant_rbac_admin(ctx: &Context<'_>) -> async_graphql::Result<()> {
    require_exact_all_scope(ctx, PERM_ROLE_MANAGE)
}

/// Tenant-wide HR employee review/configuration queues require `employee:manage=ALL`.
pub fn require_employee_manage_all(ctx: &Context<'_>) -> async_graphql::Result<()> {
    require_exact_all_scope(ctx, PERM_EMPLOYEE_MANAGE)
}
