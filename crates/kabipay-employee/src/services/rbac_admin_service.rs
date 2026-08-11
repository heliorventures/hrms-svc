//! Tenant RBAC administration (roles, permissions, `user_role`, `permission_scope`).

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::{
    permission, permission_scope, role, role_permission, user, user_role,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use uuid::Uuid;

pub async fn list_roles(db: &DatabaseConnection, tenant_id: Uuid, limit: u64) -> KabiPayResult<Vec<role::Model>> {
    let rows = role::Entity::find()
        .filter(role::Column::TenantId.eq(tenant_id))
        .filter(role::Column::IsDeleted.eq(false))
        .limit(limit.min(200))
        .all(db)
        .await?;
    Ok(rows)
}

pub async fn list_permissions(db: &DatabaseConnection, limit: u64) -> KabiPayResult<Vec<permission::Model>> {
    let rows = permission::Entity::find().limit(limit.min(500)).all(db).await?;
    Ok(rows)
}

pub async fn list_users(db: &DatabaseConnection, tenant_id: Uuid, limit: u64) -> KabiPayResult<Vec<user::Model>> {
    let rows = user::Entity::find()
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .limit(limit.min(200))
        .all(db)
        .await?;
    Ok(rows)
}

/// Login usernames and optional contact emails for linked user references on employee rows.
pub async fn map_user_login_labels_by_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, (String, Option<String>)>> {
    if ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = user::Entity::find()
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .filter(user::Column::Id.is_in(ids.to_vec()))
        .all(db)
        .await?;
    Ok(rows
        .into_iter()
        .map(|r| (r.id, (r.username, r.email)))
        .collect())
}

async fn ensure_role_in_tenant(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_id: Uuid,
) -> KabiPayResult<()> {
    let r = role::Entity::find_by_id(role_id)
        .one(db)
        .await?
        .filter(|m| m.tenant_id == tenant_id && !m.is_deleted);
    if r.is_none() {
        return Err(KabiPayError::NotFound {
            entity: "role",
            id: role_id.to_string(),
        });
    }
    Ok(())
}

async fn ensure_user_in_tenant(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<()> {
    let u = user::Entity::find_by_id(user_id)
        .one(db)
        .await?
        .filter(|m| m.tenant_id == tenant_id && !m.is_deleted);
    if u.is_none() {
        return Err(KabiPayError::NotFound {
            entity: "user",
            id: user_id.to_string(),
        });
    }
    Ok(())
}

pub async fn permission_ids_for_role(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    ensure_role_in_tenant(db, tenant_id, role_id).await?;
    let rps = role_permission::Entity::find()
        .filter(role_permission::Column::RoleId.eq(role_id))
        .all(db)
        .await?;
    Ok(rps.into_iter().map(|x| x.permission_id).collect())
}

pub async fn scopes_for_role(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_id: Uuid,
) -> KabiPayResult<Vec<permission_scope::Model>> {
    ensure_role_in_tenant(db, tenant_id, role_id).await?;
    let rows = permission_scope::Entity::find()
        .filter(permission_scope::Column::RoleId.eq(role_id))
        .filter(permission_scope::Column::TenantId.eq(tenant_id))
        .all(db)
        .await?;
    Ok(rows)
}

pub async fn role_ids_for_user(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    ensure_user_in_tenant(db, tenant_id, user_id).await?;
    let urs = user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_id))
        .all(db)
        .await?;
    Ok(urs.into_iter().map(|x| x.role_id).collect())
}

pub async fn set_role_permissions(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_id: Uuid,
    permission_ids: Vec<Uuid>,
) -> KabiPayResult<()> {
    ensure_role_in_tenant(db, tenant_id, role_id).await?;
    let unique: Vec<Uuid> = permission_ids.into_iter().collect::<HashSet<_>>().into_iter().collect();
    for pid in &unique {
        permission::Entity::find_by_id(*pid).one(db).await?.ok_or_else(|| KabiPayError::NotFound {
            entity: "permission",
            id: pid.to_string(),
        })?;
    }

    let txn = db.begin().await?;
    role_permission::Entity::delete_many()
        .filter(role_permission::Column::RoleId.eq(role_id))
        .exec(&txn)
        .await?;
    let now = Utc::now();
    for pid in unique {
        role_permission::ActiveModel {
            role_id: Set(role_id),
            permission_id: Set(pid),
            created_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    txn.commit().await?;
    Ok(())
}

pub async fn set_user_roles(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
    role_ids: Vec<Uuid>,
) -> KabiPayResult<()> {
    ensure_user_in_tenant(db, tenant_id, user_id).await?;
    let unique: Vec<Uuid> = role_ids.into_iter().collect::<HashSet<_>>().into_iter().collect();
    for rid in &unique {
        ensure_role_in_tenant(db, tenant_id, *rid).await?;
    }

    let txn = db.begin().await?;
    user_role::Entity::delete_many()
        .filter(user_role::Column::UserId.eq(user_id))
        .exec(&txn)
        .await?;
    let now = Utc::now();
    for rid in unique {
        user_role::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(rid),
            assigned_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    txn.commit().await?;
    Ok(())
}

pub async fn set_role_permission_scopes(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_id: Uuid,
    scopes: Vec<(String, String, String)>,
) -> KabiPayResult<()> {
    ensure_role_in_tenant(db, tenant_id, role_id).await?;
    let txn = db.begin().await?;
    permission_scope::Entity::delete_many()
        .filter(permission_scope::Column::TenantId.eq(tenant_id))
        .filter(permission_scope::Column::RoleId.eq(role_id))
        .exec(&txn)
        .await?;
    let now = Utc::now();
    for (resource, action, scope_type) in scopes {
        let st = scope_type.trim().to_ascii_uppercase();
        permission_scope::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            role_id: Set(role_id),
            resource: Set(resource.trim().to_string()),
            action: Set(action.trim().to_string()),
            scope_type: Set(st),
            created_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    txn.commit().await?;
    Ok(())
}
