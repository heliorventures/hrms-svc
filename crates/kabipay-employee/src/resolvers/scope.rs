//! Shared M3 data-scope + viewer helpers (employee list / document access).

use async_graphql::Context;
use kabipay_common::client_data_scope::data_scope_from_context;
use kabipay_common::context::ScopeType;
use kabipay_common::context::PERM_EMPLOYEE_READ;
use kabipay_common::subgraph::require_client_claims;
use kabipay_common::KabiPayError;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

use crate::services::employee_service;

pub use kabipay_common::client_data_scope::resolve_viewer_employee;

/// Dev `x-tenant-id` without JWT uses `All` (unchanged for probes).
pub fn data_scope_employee(ctx: &Context<'_>) -> async_graphql::Result<ScopeType> {
    data_scope_from_context(ctx, PERM_EMPLOYEE_READ)
}

/// `employee(id)`-style visibility: target employee row must be in caller’s `employee` data scope.
pub async fn assert_employee_in_data_scope(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_emp_id: Uuid,
) -> async_graphql::Result<()> {
    let scope = data_scope_employee(ctx)?;
    let Some(target) = employee_service::find_by_id(db, tenant_id, target_emp_id)
        .await
        .map_err(KabiPayError::into_graphql)?
    else {
        return Err(KabiPayError::NotFound {
            entity: "employee",
            id: target_emp_id.to_string(),
        }
        .into_graphql());
    };
    let viewer = resolve_viewer_employee(ctx, db, tenant_id).await?;
    if !employee_service::is_employee_in_scope(scope, viewer, &target) {
        return Err(KabiPayError::Forbidden(
            "not allowed to access this employee for documents".into(),
        )
        .into_graphql());
    }
    Ok(())
}

/// Tenant RBAC administration requires the exact `role:manage` permission.
pub fn require_tenant_rbac_admin(ctx: &Context<'_>) -> async_graphql::Result<()> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_tenant_rbac() {
        return Err(KabiPayError::Forbidden(
            "role:manage permission is required".into(),
        )
        .into_graphql());
    }
    Ok(())
}
