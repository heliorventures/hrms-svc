//! Both the atomic transport and compatibility transports use this transaction.
use super::*;
use crate::services::rbac_access_policy::validate_access;

#[cfg(test)]
#[path = "rbac_access_configuration_tests.rs"]
mod tests;

pub async fn set_role_access_for_actor(
    db: &DatabaseConnection,
    tenant: Uuid,
    role_id: Uuid,
    permission_ids: Vec<Uuid>,
    scopes: Vec<(String, String, String)>,
    actor: Option<Uuid>,
) -> KabiPayResult<()> {
    replace_access(
        db,
        tenant,
        role_id,
        Some(permission_ids),
        Some(scopes),
        actor,
    )
    .await
}

pub(super) async fn replace_access(
    db: &DatabaseConnection,
    tenant: Uuid,
    role_id: Uuid,
    permission_ids: Option<Vec<Uuid>>,
    scopes: Option<Vec<(String, String, String)>>,
    actor: Option<Uuid>,
) -> KabiPayResult<()> {
    let tx = db.begin().await?;
    let result: KabiPayResult<()> = async {
        let before = begin_role_authority_change(&tx, tenant, actor).await?;
        role::Entity::find_by_id(role_id)
            .filter(role::Column::TenantId.eq(tenant))
            .filter(role::Column::IsDeleted.eq(false))
            .lock_exclusive().one(&tx).await?
            .filter(|row| row.tenant_id == tenant && !row.is_deleted)
            .ok_or_else(|| KabiPayError::NotFound { entity: "role", id: role_id.to_string() })?;
        let previous_ids = role_permission::Entity::find()
            .filter(role_permission::Column::RoleId.eq(role_id)).all(&tx).await?
            .into_iter().map(|row| row.permission_id).collect::<HashSet<_>>();
        let previous_scopes = permission_scope::Entity::find()
            .filter(permission_scope::Column::TenantId.eq(tenant))
            .filter(permission_scope::Column::RoleId.eq(role_id)).all(&tx).await?;
        let selected_ids = permission_ids.map(|ids| ids.into_iter().collect::<HashSet<_>>())
            .unwrap_or_else(|| previous_ids.clone());
        let all_ids = previous_ids.union(&selected_ids).copied().collect::<Vec<_>>();
        let catalog = if all_ids.is_empty() { Vec::new() } else {
            permission::Entity::find().filter(permission::Column::Id.is_in(all_ids)).all(&tx).await?
        };
        let keys_by_id = catalog.into_iter().map(|p| (p.id, format!("{}:{}", p.resource, p.action))).collect::<HashMap<_, _>>();
        let selected = selected_ids.iter().map(|id| keys_by_id.get(id).cloned()
            .ok_or_else(|| KabiPayError::NotFound { entity: "permission", id: id.to_string() }))
            .collect::<KabiPayResult<HashSet<_>>>()?;
        let previously_scoped = previous_scopes.iter().map(|s| format!("{}:{}", s.resource, s.action)).collect::<HashSet<_>>();
        let previous_keys = previous_ids.iter().filter_map(|id| keys_by_id.get(id)).cloned().collect::<HashSet<_>>();
        let retained_inactive = previous_keys.difference(&previously_scoped).cloned().collect::<HashSet<_>>();
        // The permissions-only compatibility endpoint retains matching scopes and
        // discards scopes for removed grants; it cannot create an unscoped grant.
        let requested_scopes = scopes.unwrap_or_else(|| previous_scopes.into_iter()
            .filter(|row| {
                let key = format!("{}:{}", row.resource, row.action);
                selected.contains(&key) && previous_keys.contains(&key)
            })
            .map(|row| (row.resource, row.action, row.scope_type)).collect());
        let scopes = requested_scopes.into_iter().map(|(resource, action, scope)|
            (resource.trim().to_string(), action.trim().to_string(), scope.trim().to_ascii_uppercase()))
            .collect::<Vec<_>>();
        validate_access(&selected, &scopes, &retained_inactive).map_err(KabiPayError::Validation)?;

        role_permission::Entity::delete_many().filter(role_permission::Column::RoleId.eq(role_id)).exec(&tx).await?;
        permission_scope::Entity::delete_many()
            .filter(permission_scope::Column::TenantId.eq(tenant))
            .filter(permission_scope::Column::RoleId.eq(role_id)).exec(&tx).await?;
        let now = Utc::now();
        for id in selected_ids {
            role_permission::Entity::insert_many([role_permission::ActiveModel { role_id: Set(role_id), permission_id: Set(id), created_at: Set(now) }]).exec_without_returning(&tx).await?;
        }
        for (resource, action, scope_type) in scopes {
            permission_scope::Entity::insert_many([permission_scope::ActiveModel {
                id: Set(Uuid::new_v4()), tenant_id: Set(tenant), role_id: Set(role_id),
                resource: Set(resource), action: Set(action), scope_type: Set(scope_type), created_at: Set(now),
            }]).exec_without_returning(&tx).await?;
        }
        finish_role_authority_change(&tx, tenant, actor, before).await?;
        // The existing role_permission/permission_scope triggers advance assigned
        // account versions. Revoke refresh sessions in the same transaction.
        tx.execute(kabipay_organization_tenant::statement(
            "DELETE FROM user_session WHERE user_id IN (SELECT u.id FROM \"user\" u JOIN user_role ur ON ur.user_id=u.id WHERE u.tenant_id=$1 AND ur.role_id=$2)",
            vec![tenant.into(), role_id.into()],
        )).await?;
        Ok(())
    }.await;
    match result {
        Ok(()) => tx.commit().await.map_err(Into::into),
        Err(error) => {
            tx.rollback().await?;
            Err(error)
        }
    }
}
