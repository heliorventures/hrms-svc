//! Resolve tenant RBAC for JWT claims: roles, permissions, and exact permission scopes.

use std::collections::{BTreeSet, HashMap};

use kabipay_common::context::ScopeType;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::{
    permission, permission_scope, role, role_permission, user_role,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

pub struct ClientAuthorization {
    pub roles: Vec<String>,
    pub permissions: Vec<String>,
    pub permission_scopes: HashMap<String, String>,
    pub resource_scopes: HashMap<String, String>,
}

fn merge_permission_scope(
    best: &mut HashMap<String, ScopeType>,
    permission: String,
    candidate: ScopeType,
) {
    best.entry(permission)
        .and_modify(|current| {
            if candidate.rank() > current.rank() {
                *current = candidate;
            }
        })
        .or_insert(candidate);
}

fn empty_client_authorization() -> ClientAuthorization {
    ClientAuthorization {
        roles: Vec::new(),
        permissions: Vec::new(),
        permission_scopes: HashMap::new(),
        resource_scopes: HashMap::new(),
    }
}

fn permission_code(row: &permission::Model) -> String {
    format!("{}:{}", row.resource, row.action).to_ascii_lowercase()
}

fn build_client_authorization(
    tenant_id: Uuid,
    role_rows: Vec<role::Model>,
    role_permissions: Vec<role_permission::Model>,
    permission_scope_rows: Vec<permission_scope::Model>,
    permission_rows: Vec<permission::Model>,
) -> ClientAuthorization {
    // The role schema represents inactive roles through soft deletion; there is no separate
    // `is_active` column. Recheck tenancy and deletion here as defense in depth around the query.
    let effective_roles = role_rows
        .into_iter()
        .filter(|row| row.tenant_id == tenant_id && !row.is_deleted)
        .collect::<Vec<_>>();
    if effective_roles.is_empty() {
        return empty_client_authorization();
    }

    let effective_role_ids = effective_roles
        .iter()
        .map(|row| row.id)
        .collect::<BTreeSet<_>>();
    let roles = effective_roles
        .into_iter()
        .map(|row| row.name)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();
    let permission_codes_by_id = permission_rows
        .iter()
        .map(|row| (row.id, permission_code(row)))
        .collect::<HashMap<_, _>>();
    let grants_by_role = role_permissions
        .into_iter()
        .filter(|row| effective_role_ids.contains(&row.role_id))
        .filter_map(|row| {
            permission_codes_by_id
                .get(&row.permission_id)
                .cloned()
                .map(|permission| (row.role_id, permission))
        })
        .collect::<BTreeSet<_>>();
    let permissions = grants_by_role
        .iter()
        .map(|(_, permission)| permission.clone())
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

    let mut best_permission_scopes = HashMap::new();
    for row in permission_scope_rows {
        if row.tenant_id != tenant_id || !effective_role_ids.contains(&row.role_id) {
            continue;
        }
        let exact_permission =
            format!("{}:{}", row.resource, row.action).to_ascii_lowercase();
        if !grants_by_role.contains(&(row.role_id, exact_permission.clone())) {
            continue;
        }
        let Some(scope) = ScopeType::parse_loose(&row.scope_type) else {
            continue;
        };
        merge_permission_scope(&mut best_permission_scopes, exact_permission, scope);
    }

    let permission_scopes = best_permission_scopes
        .into_iter()
        .map(|(permission, scope)| (permission, scope.to_wire().to_string()))
        .collect();

    ClientAuthorization {
        roles,
        permissions,
        permission_scopes,
        // Kept on the wire for token compatibility only. New authorization is exact-permission
        // based and must not derive a broad resource fallback.
        resource_scopes: HashMap::new(),
    }
}

/// Resolve all client authorization claims for `user_id` with one user-role
/// read. The connection is already schema-scoped; tenant filters are still
/// applied where the table carries `tenant_id`.
pub async fn load_client_authorization(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<ClientAuthorization> {
    let user_roles = user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_id))
        .all(db)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    if user_roles.is_empty() {
        return Ok(empty_client_authorization());
    }

    let assigned_role_ids = user_roles
        .into_iter()
        .map(|row| row.role_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let role_rows = role::Entity::find()
        .filter(role::Column::TenantId.eq(tenant_id))
        .filter(role::Column::Id.is_in(assigned_role_ids))
        .filter(role::Column::IsDeleted.eq(false))
        .all(db)
        .await
        .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;
    if role_rows.is_empty() {
        return Ok(empty_client_authorization());
    }

    let effective_role_ids = role_rows.iter().map(|row| row.id).collect::<Vec<_>>();
    let role_permissions = role_permission::Entity::find()
        .filter(role_permission::Column::RoleId.is_in(effective_role_ids.clone()))
        .all(db);
    let permission_scope_rows = permission_scope::Entity::find()
        .filter(permission_scope::Column::TenantId.eq(tenant_id))
        .filter(permission_scope::Column::RoleId.is_in(effective_role_ids))
        .all(db);

    let (role_permissions, permission_scope_rows) =
        tokio::try_join!(role_permissions, permission_scope_rows)
            .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;

    let permission_ids = role_permissions
        .iter()
        .map(|row| row.permission_id)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect::<Vec<_>>();
    let permission_rows = if permission_ids.is_empty() {
        Vec::new()
    } else {
        permission::Entity::find()
            .filter(permission::Column::Id.is_in(permission_ids))
            .all(db)
            .await
            .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?
    };

    Ok(build_client_authorization(
        tenant_id,
        role_rows,
        role_permissions,
        permission_scope_rows,
        permission_rows,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;

    fn role_row(id: Uuid, tenant_id: Uuid, name: &str, is_deleted: bool) -> role::Model {
        role::Model {
            id,
            tenant_id,
            name: name.into(),
            description: None,
            is_system_role: true,
            is_deleted,
            deleted_at: None,
            deleted_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn permission_row(id: Uuid, resource: &str, action: &str) -> permission::Model {
        permission::Model {
            id,
            resource: resource.into(),
            action: action.into(),
            module_id: Uuid::new_v4(),
            description: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    fn role_grant(role_id: Uuid, permission_id: Uuid) -> role_permission::Model {
        role_permission::Model {
            role_id,
            permission_id,
            created_at: Utc::now(),
        }
    }

    fn scope_row(
        tenant_id: Uuid,
        role_id: Uuid,
        resource: &str,
        action: &str,
        scope_type: &str,
    ) -> permission_scope::Model {
        permission_scope::Model {
            id: Uuid::new_v4(),
            tenant_id,
            role_id,
            resource: resource.into(),
            action: action.into(),
            scope_type: scope_type.into(),
            created_at: Utc::now(),
        }
    }

    #[test]
    fn scope_on_another_role_cannot_broaden_a_granted_permission() {
        let tenant_id = Uuid::new_v4();
        let granting_role_id = Uuid::new_v4();
        let scoped_role_id = Uuid::new_v4();
        let leave_read_id = Uuid::new_v4();
        let expense_read_id = Uuid::new_v4();

        let authorization = build_client_authorization(
            tenant_id,
            vec![
                role_row(granting_role_id, tenant_id, "EMPLOYEE", false),
                role_row(scoped_role_id, tenant_id, "MANAGER", false),
            ],
            vec![
                role_grant(granting_role_id, leave_read_id),
                role_grant(scoped_role_id, expense_read_id),
            ],
            vec![scope_row(
                tenant_id,
                scoped_role_id,
                "leave",
                "read",
                "ALL",
            )],
            vec![
                permission_row(leave_read_id, "leave", "read"),
                permission_row(expense_read_id, "expense", "read"),
            ],
        );

        assert_eq!(
            authorization.permissions,
            vec!["expense:read", "leave:read"]
        );
        assert!(!authorization.permission_scopes.contains_key("leave:read"));
        assert!(authorization.resource_scopes.is_empty());
    }

    #[test]
    fn inactive_deleted_and_cross_tenant_roles_contribute_nothing() {
        let tenant_id = Uuid::new_v4();
        let other_tenant_id = Uuid::new_v4();
        let active_role_id = Uuid::new_v4();
        let deleted_role_id = Uuid::new_v4();
        let cross_tenant_role_id = Uuid::new_v4();
        let leave_read_id = Uuid::new_v4();
        let expense_read_id = Uuid::new_v4();
        let payroll_read_id = Uuid::new_v4();

        let authorization = build_client_authorization(
            tenant_id,
            vec![
                role_row(active_role_id, tenant_id, "EMPLOYEE", false),
                role_row(deleted_role_id, tenant_id, "DELETED", true),
                role_row(
                    cross_tenant_role_id,
                    other_tenant_id,
                    "FOREIGN",
                    false,
                ),
            ],
            vec![
                role_grant(active_role_id, leave_read_id),
                role_grant(deleted_role_id, expense_read_id),
                role_grant(cross_tenant_role_id, payroll_read_id),
            ],
            vec![
                scope_row(tenant_id, active_role_id, "leave", "read", "SELF"),
                scope_row(tenant_id, deleted_role_id, "expense", "read", "ALL"),
                scope_row(
                    other_tenant_id,
                    cross_tenant_role_id,
                    "payroll",
                    "read",
                    "ALL",
                ),
            ],
            vec![
                permission_row(leave_read_id, "leave", "read"),
                permission_row(expense_read_id, "expense", "read"),
                permission_row(payroll_read_id, "payroll", "read"),
            ],
        );

        assert_eq!(authorization.roles, vec!["EMPLOYEE"]);
        assert_eq!(authorization.permissions, vec!["leave:read"]);
        assert_eq!(
            authorization
                .permission_scopes
                .get("leave:read")
                .map(String::as_str),
            Some("SELF")
        );
        assert!(authorization.resource_scopes.is_empty());
    }

    #[test]
    fn same_permission_merges_to_widest_scope_without_broadening_other_actions() {
        let tenant_id = Uuid::new_v4();
        let employee_role_id = Uuid::new_v4();
        let manager_role_id = Uuid::new_v4();
        let leave_read_id = Uuid::new_v4();
        let expense_read_id = Uuid::new_v4();

        let authorization = build_client_authorization(
            tenant_id,
            vec![
                role_row(employee_role_id, tenant_id, "EMPLOYEE", false),
                role_row(manager_role_id, tenant_id, "MANAGER", false),
            ],
            vec![
                role_grant(employee_role_id, leave_read_id),
                role_grant(manager_role_id, leave_read_id),
                role_grant(employee_role_id, expense_read_id),
            ],
            vec![
                scope_row(
                    tenant_id,
                    employee_role_id,
                    "leave",
                    "read",
                    "SELF",
                ),
                scope_row(
                    tenant_id,
                    manager_role_id,
                    "leave",
                    "read",
                    "TEAM",
                ),
                scope_row(
                    tenant_id,
                    employee_role_id,
                    "expense",
                    "read",
                    "SELF",
                ),
                scope_row(
                    tenant_id,
                    manager_role_id,
                    "expense",
                    "read",
                    "ALL",
                ),
            ],
            vec![
                permission_row(leave_read_id, "leave", "read"),
                permission_row(expense_read_id, "expense", "read"),
            ],
        );

        assert_eq!(
            authorization
                .permission_scopes
                .get("leave:read")
                .map(String::as_str),
            Some("TEAM")
        );
        assert_eq!(
            authorization
                .permission_scopes
                .get("expense:read")
                .map(String::as_str),
            Some("SELF")
        );
        assert!(authorization.resource_scopes.is_empty());
    }

    #[test]
    fn malformed_scope_remains_absent_for_a_granted_permission() {
        let tenant_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let leave_read_id = Uuid::new_v4();

        let authorization = build_client_authorization(
            tenant_id,
            vec![role_row(role_id, tenant_id, "EMPLOYEE", false)],
            vec![role_grant(role_id, leave_read_id)],
            vec![scope_row(
                tenant_id,
                role_id,
                "leave",
                "read",
                "INVALID",
            )],
            vec![permission_row(leave_read_id, "leave", "read")],
        );

        assert_eq!(authorization.permissions, vec!["leave:read"]);
        assert!(authorization.permission_scopes.is_empty());
        assert!(authorization.resource_scopes.is_empty());
    }

    #[test]
    fn no_effective_roles_return_no_authorization_claims() {
        let tenant_id = Uuid::new_v4();
        let deleted_role_id = Uuid::new_v4();
        let foreign_role_id = Uuid::new_v4();
        let leave_read_id = Uuid::new_v4();

        let authorization = build_client_authorization(
            tenant_id,
            vec![
                role_row(deleted_role_id, tenant_id, "DELETED", true),
                role_row(
                    foreign_role_id,
                    Uuid::new_v4(),
                    "FOREIGN",
                    false,
                ),
            ],
            vec![
                role_grant(deleted_role_id, leave_read_id),
                role_grant(foreign_role_id, leave_read_id),
            ],
            vec![scope_row(
                tenant_id,
                deleted_role_id,
                "leave",
                "read",
                "ALL",
            )],
            vec![permission_row(leave_read_id, "leave", "read")],
        );

        assert!(authorization.roles.is_empty());
        assert!(authorization.permissions.is_empty());
        assert!(authorization.permission_scopes.is_empty());
        assert!(authorization.resource_scopes.is_empty());
    }
}
