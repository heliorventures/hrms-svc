//! JWT exact-permission scope helpers for list filters keyed by `employee_id`.

use async_graphql::Context;

use crate::context::{
    ClientClaims, ClientViewerEmployee, ScopeType, EMPLOYMENT_STATUS_ACTIVE,
    EMPLOYMENT_STATUS_PROBATION,
};
use crate::error::{KabiPayError, KabiPayResult};
use crate::subgraph::resolve_client_employee_id_with_connection;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    QuerySelect, Statement,
};
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
    claims.scope_for_permission(permission).ok_or_else(|| {
        KabiPayError::Forbidden(format!(
            "{permission} permission requires an explicit valid scope"
        ))
    })
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

    fn claims_with_permission_scope(scope: Option<&str>) -> ClientClaims {
        let mut permission_scopes = std::collections::HashMap::new();
        if let Some(scope) = scope {
            permission_scopes.insert("attendance:read".into(), scope.into());
        }
        ClientClaims {
            sub: Uuid::nil(),
            iss: crate::context::CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::nil(),
            email: String::new(),
            employee_id: None,
            must_change_password: false,
            roles: vec![],
            permissions: vec!["attendance:read".into()],
            permission_scopes,
            resource_scopes: Default::default(),
        }
    }

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

    #[test]
    fn protected_scope_lookup_rejects_missing_or_malformed_exact_scope() {
        for claims in [
            claims_with_permission_scope(None),
            claims_with_permission_scope(Some("INVALID")),
        ] {
            let error = data_scope_from_claims(Some(&claims), "attendance:read")
                .expect_err("scoped access must require an explicit valid exact scope");

            assert!(matches!(error, KabiPayError::Forbidden(_)));
        }
    }

    #[test]
    fn protected_scope_lookup_does_not_reuse_a_sibling_action_scope() {
        let mut claims = claims_with_permission_scope(None);
        claims
            .permission_scopes
            .insert("attendance:regularize".into(), "ALL".into());
        claims
            .resource_scopes
            .insert(crate::context::SCOPE_RES_ATTENDANCE.into(), "ALL".into());

        let error = data_scope_from_claims(Some(&claims), "attendance:read")
            .expect_err("attendance:read must have its own explicit scope");

        assert!(matches!(error, KabiPayError::Forbidden(_)));
    }

    #[test]
    fn recursive_team_statement_is_tenant_bound_active_nondeleted_id_only_and_cycle_safe() {
        let tenant_id = Uuid::new_v4();
        let manager_id = Uuid::new_v4();

        let statement = recursive_team_employee_ids_statement(tenant_id, manager_id);
        let normalized = statement
            .sql
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");

        assert!(normalized.starts_with("WITH RECURSIVE team(employee_id, traversal_path) AS"));
        assert!(normalized.contains("root.tenant_id = $1"));
        assert!(normalized.contains("root.is_deleted = FALSE"));
        assert!(normalized.contains("root.status IN ($3, $4)"));
        assert!(normalized.contains("child.tenant_id = $1"));
        assert!(normalized.contains("child.is_deleted = FALSE"));
        assert!(normalized.contains("child.status IN ($3, $4)"));
        assert!(normalized.contains("NOT child.id = ANY(team.traversal_path)"));
        assert!(normalized.contains("SELECT DISTINCT employee_id AS id FROM team"));
        assert!(!normalized.contains("first_name"));
        assert!(!normalized.contains("last_name"));
        assert!(!normalized.contains("SELECT *"));
        assert_eq!(statement.values.as_ref().expect("bound values").0.len(), 4);
        assert!(!normalized.contains("'ACTIVE'"));
        assert!(!normalized.contains("'PROBATION'"));
    }

    #[test]
    fn resolved_filters_enforce_self_team_and_all_target_membership() {
        let viewer_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();

        let self_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id]);
        let team_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id, descendant_id]);

        assert!(self_filter.allows_employee(viewer_id));
        assert!(!self_filter.allows_employee(descendant_id));
        assert!(team_filter.allows_employee(viewer_id));
        assert!(team_filter.allows_employee(descendant_id));
        assert!(!team_filter.allows_employee(outside_id));
        assert!(EmployeeScopeFilter::Unrestricted.allows_employee(outside_id));
    }

    #[tokio::test]
    async fn self_and_team_scopes_remain_empty_without_a_viewer() {
        for scope in [ScopeType::Self_, ScopeType::Team] {
            let filter = resolve_employee_scope_filter(
                &DatabaseConnection::Disconnected,
                Uuid::new_v4(),
                scope,
                None,
            )
            .await
            .expect("missing viewer must not require a database query");

            assert!(matches!(filter, EmployeeScopeFilter::Empty));
        }
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

fn recursive_team_employee_ids_statement(
    tenant_id: Uuid,
    manager_employee_id: Uuid,
) -> Statement {
    Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"WITH RECURSIVE team(employee_id, traversal_path) AS (
               SELECT root.id, ARRAY[root.id]
               FROM employee AS root
               WHERE root.tenant_id = $1
                 AND root.id = $2
                 AND root.is_deleted = FALSE
                 AND root.status IN ($3, $4)
               UNION ALL
               SELECT child.id, team.traversal_path || child.id
               FROM employee AS child
               INNER JOIN team ON child.reporting_manager_id = team.employee_id
               WHERE child.tenant_id = $1
                 AND child.is_deleted = FALSE
                 AND child.status IN ($3, $4)
                 AND NOT child.id = ANY(team.traversal_path)
           )
           SELECT DISTINCT employee_id AS id
           FROM team
           ORDER BY employee_id"#,
        vec![
            tenant_id.into(),
            manager_employee_id.into(),
            EMPLOYMENT_STATUS_ACTIVE.into(),
            EMPLOYMENT_STATUS_PROBATION.into(),
        ],
    )
}

async fn recursive_team_employee_ids<C>(
    db: &C,
    tenant_id: Uuid,
    manager_employee_id: Uuid,
) -> KabiPayResult<Vec<Uuid>>
where
    C: ConnectionTrait + Sync,
{
    db.query_all(recursive_team_employee_ids_statement(
        tenant_id,
        manager_employee_id,
    ))
    .await?
    .into_iter()
    .map(|row| row.try_get("", "id").map_err(KabiPayError::from))
    .collect()
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
            let ids = recursive_team_employee_ids(db, tenant_id, v.employee_id).await?;
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
                .select_only()
                .column(employee::Column::Id)
                .filter(employee::Column::TenantId.eq(tenant_id))
                .filter(employee::Column::IsDeleted.eq(false))
                .filter(employee::Column::DepartmentId.eq(Some(d)))
                .into_tuple::<Uuid>()
                .all(db)
                .await?;
            Ok(EmployeeScopeFilter::EmployeeIds(ids))
        }
    }
}
