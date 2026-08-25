//! Resolve tenant RBAC for JWT claims: roles, permissions, and resource scopes.

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

fn merge_resource_scope(
    best: &mut HashMap<String, ScopeType>,
    resource: String,
    candidate: ScopeType,
) {
    best.entry(resource)
        .and_modify(|current| {
            if candidate.rank() > current.rank() {
                *current = candidate;
            }
        })
        .or_insert(candidate);
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
        return Ok(ClientAuthorization {
            roles: vec![],
            permissions: vec![],
            permission_scopes: HashMap::new(),
            resource_scopes: HashMap::new(),
        });
    }

    let role_ids: Vec<Uuid> = user_roles.iter().map(|row| row.role_id).collect();

    let role_rows = role::Entity::find()
        .filter(role::Column::TenantId.eq(tenant_id))
        .filter(role::Column::Id.is_in(role_ids.clone()))
        .filter(role::Column::IsDeleted.eq(false))
        .all(db);
    let role_permissions = role_permission::Entity::find()
        .filter(role_permission::Column::RoleId.is_in(role_ids.clone()))
        .all(db);
    let permission_scope_rows = permission_scope::Entity::find()
        .filter(permission_scope::Column::TenantId.eq(tenant_id))
        .filter(permission_scope::Column::RoleId.is_in(role_ids))
        .all(db);

    let (role_rows, role_permissions, permission_scope_rows) =
        tokio::try_join!(role_rows, role_permissions, permission_scope_rows)
            .map_err(|error| KabiPayError::from_tenant_db(tenant_id, error))?;

    let roles = role_rows
        .into_iter()
        .map(|row| row.name)
        .collect::<BTreeSet<_>>()
        .into_iter()
        .collect();

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

    let granted_permissions = permission_rows
        .iter()
        .map(|row| format!("{}:{}", row.resource, row.action).to_ascii_lowercase())
        .collect::<BTreeSet<_>>();
    let permissions = granted_permissions.iter().cloned().collect();

    let mut best_permission_scopes: HashMap<String, ScopeType> = HashMap::new();
    let mut best_resource_scopes: HashMap<String, ScopeType> = HashMap::new();
    for row in permission_scope_rows {
        let permission = format!("{}:{}", row.resource, row.action).to_ascii_lowercase();
        if !granted_permissions.contains(&permission) {
            continue;
        }
        let Some(scope) = ScopeType::parse_loose(&row.scope_type) else {
            continue;
        };
        merge_permission_scope(&mut best_permission_scopes, permission, scope);
        merge_resource_scope(
            &mut best_resource_scopes,
            row.resource.to_ascii_lowercase(),
            scope,
        );
    }

    let permission_scopes = best_permission_scopes
        .into_iter()
        .map(|(permission, scope)| (permission, scope.to_wire().to_string()))
        .collect();
    let resource_scopes = best_resource_scopes
        .into_iter()
        .map(|(resource, scope)| (resource, scope.to_wire().to_string()))
        .collect();

    Ok(ClientAuthorization {
        roles,
        permissions,
        permission_scopes,
        resource_scopes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resource_scope_keeps_widest_value() {
        let mut scopes = HashMap::new();
        merge_resource_scope(&mut scopes, "employee".into(), ScopeType::Self_);
        merge_resource_scope(&mut scopes, "employee".into(), ScopeType::All);
        merge_resource_scope(&mut scopes, "employee".into(), ScopeType::Team);
        assert_eq!(scopes.get("employee"), Some(&ScopeType::All));
    }

    #[test]
    fn permission_scopes_do_not_broaden_other_actions() {
        let mut scopes = HashMap::new();
        merge_permission_scope(
            &mut scopes,
            "attendance:read".into(),
            ScopeType::All,
        );
        merge_permission_scope(
            &mut scopes,
            "attendance:regularize".into(),
            ScopeType::Self_,
        );

        assert_eq!(scopes.get("attendance:read"), Some(&ScopeType::All));
        assert_eq!(
            scopes.get("attendance:regularize"),
            Some(&ScopeType::Self_)
        );
    }
}
