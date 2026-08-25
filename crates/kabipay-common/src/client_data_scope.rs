//! M3 / M10: JWT **`resource_scopes`** helpers for list filters keyed by `employee_id`.

use async_graphql::Context;

use crate::context::{ClientClaims, ClientViewerEmployee, ScopeType};
use crate::error::{KabiPayError, KabiPayResult};
use crate::subgraph::resolve_client_employee_id_with_connection;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

/// Dev / probe: no JWT → treat as **ALL** (unchanged from subgraph conventions).
pub fn data_scope_from_claims(
    claims: Option<&ClientClaims>,
    permission: &str,
) -> KabiPayResult<ScopeType> {
    let claims = claims.ok_or(KabiPayError::Unauthorised)?;
    if !claims.has_any_permission(&[permission]) {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission required"
        )));
    }
    Ok(claims.scope_for_permission(permission))
}

pub fn data_scope_from_context(
    ctx: &Context<'_>,
    permission: &str,
) -> async_graphql::Result<ScopeType> {
    data_scope_from_claims(ctx.data_opt::<ClientClaims>(), permission)
        .map_err(KabiPayError::into_graphql)
}

/// Caller’s employee row for **TEAM** / **DEPARTMENT** filters. **`None`** when unauthenticated
/// or user has no linked employee.
pub async fn resolve_viewer_employee(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> async_graphql::Result<Option<ClientViewerEmployee>> {
    resolve_viewer_employee_with_connection(ctx, db, tenant_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn protected_scope_lookup_rejects_missing_claims() {
        let error = data_scope_from_claims(None, "attendance:read").unwrap_err();
        assert!(matches!(error, KabiPayError::Unauthorised));
    }

    #[test]
    fn protected_scope_lookup_rejects_claims_without_the_requested_permission() {
        let claims = ClientClaims {
            sub: Uuid::nil(),
            iss: crate::context::CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::nil(),
            email: String::new(),
            employee_id: None,
            must_change_password: false,
            roles: vec!["TENANT_ADMIN".into()],
            permissions: vec![],
            permission_scopes: Default::default(),
            resource_scopes: Default::default(),
        };

        let error = data_scope_from_claims(Some(&claims), "attendance:read").unwrap_err();
        assert!(matches!(error, KabiPayError::Forbidden(_)));
    }
}

/// Connection-generic form of [`resolve_viewer_employee`] for callers that
/// must resolve a viewer inside their existing transaction.
pub async fn resolve_viewer_employee_with_connection<C>(
    ctx: &Context<'_>,
    db: &C,
    tenant_id: Uuid,
) -> async_graphql::Result<Option<ClientViewerEmployee>>
where
    C: ConnectionTrait + Sync,
{
    if ctx.data_opt::<ClientClaims>().is_none() {
        return Ok(None);
    }
    let Ok(emp_id) = resolve_client_employee_id_with_connection(ctx, db, tenant_id).await else {
        return Ok(None);
    };
    let Some(emp) = employee::Entity::find_by_id(emp_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(|e: sea_orm::DbErr| KabiPayError::from(e).into_graphql())?
    else {
        return Ok(None);
    };
    Ok(Some(ClientViewerEmployee {
        employee_id: emp.id,
        department_id: emp.department_id,
    }))
}

/// Same rule as `kabipay-employee` **`is_employee_in_scope`** (Gap H).
pub fn employee_model_in_scope(
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
    target: &employee::Model,
) -> bool {
    match scope {
        ScopeType::All => true,
        ScopeType::Self_ => {
            let Some(v) = viewer else {
                return false;
            };
            target.id == v.employee_id
        }
        ScopeType::Team => {
            let Some(v) = viewer else {
                return false;
            };
            target.id == v.employee_id || target.reporting_manager_id == Some(v.employee_id)
        }
        ScopeType::Department => {
            let Some(v) = viewer else {
                return false;
            };
            if target.id == v.employee_id {
                return true;
            }
            match (v.department_id, target.department_id) {
                (Some(d1), Some(d2)) if d1 == d2 => true,
                _ => false,
            }
        }
    }
}

/// Restrict queries with an **`employee_id`** FK (expense, attendance, payslip, …).
#[derive(Debug, Clone)]
pub enum EmployeeScopeFilter {
    Unrestricted,
    /// No rows should be returned (`WHERE 1=0` equivalent).
    Empty,
    EmployeeIds(Vec<Uuid>),
}

impl EmployeeScopeFilter {
    pub fn allows_employee(&self, employee_id: Uuid) -> bool {
        match self {
            EmployeeScopeFilter::Unrestricted => true,
            EmployeeScopeFilter::Empty => false,
            EmployeeScopeFilter::EmployeeIds(ids) => ids.contains(&employee_id),
        }
    }
}

pub async fn resolve_employee_scope_filter(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<EmployeeScopeFilter> {
    resolve_employee_scope_filter_with_connection(db, tenant_id, scope, viewer).await
}

/// Connection-generic form of [`resolve_employee_scope_filter`] for callers
/// that must resolve an employee scope inside their existing transaction.
pub async fn resolve_employee_scope_filter_with_connection<C>(
    db: &C,
    tenant_id: Uuid,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<EmployeeScopeFilter>
where
    C: ConnectionTrait + Sync,
{
    match scope {
        ScopeType::All => Ok(EmployeeScopeFilter::Unrestricted),
        ScopeType::Self_ => {
            let Some(v) = viewer else {
                return Ok(EmployeeScopeFilter::Empty);
            };
            Ok(EmployeeScopeFilter::EmployeeIds(vec![v.employee_id]))
        }
        ScopeType::Team => {
            let Some(v) = viewer else {
                return Ok(EmployeeScopeFilter::Empty);
            };
            let mut ids: Vec<Uuid> = employee::Entity::find()
                .filter(employee::Column::TenantId.eq(tenant_id))
                .filter(employee::Column::IsDeleted.eq(false))
                .filter(employee::Column::ReportingManagerId.eq(v.employee_id))
                .all(db)
                .await?
                .into_iter()
                .map(|e| e.id)
                .collect();
            ids.push(v.employee_id);
            Ok(EmployeeScopeFilter::EmployeeIds(ids))
        }
        ScopeType::Department => {
            let Some(v) = viewer else {
                return Ok(EmployeeScopeFilter::Empty);
            };
            let Some(d) = v.department_id else {
                return Ok(EmployeeScopeFilter::EmployeeIds(vec![v.employee_id]));
            };
            let ids: Vec<Uuid> = employee::Entity::find()
                .filter(employee::Column::TenantId.eq(tenant_id))
                .filter(employee::Column::IsDeleted.eq(false))
                .filter(employee::Column::DepartmentId.eq(Some(d)))
                .all(db)
                .await?
                .into_iter()
                .map(|e| e.id)
                .collect();
            Ok(EmployeeScopeFilter::EmployeeIds(ids))
        }
    }
}
