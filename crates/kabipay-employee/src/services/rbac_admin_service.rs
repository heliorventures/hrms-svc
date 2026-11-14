//! Tenant RBAC administration (roles, permissions, `user_role`, `permission_scope`).

use std::collections::{HashMap, HashSet};

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::{
    permission, permission_scope, role, role_permission, user, user_role, user_session,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, QueryFilter, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

const ACTIVE_LOGIN_ROLE_REQUIRED_CODE: &str = "ACTIVE_LOGIN_ROLE_REQUIRED";

#[path = "rbac_access_configuration.rs"]
mod access_configuration;
pub use access_configuration::set_role_access_for_actor;

fn active_login_role_required() -> KabiPayError {
    KabiPayError::BusinessRule {
        code: ACTIVE_LOGIN_ROLE_REQUIRED_CODE,
        message: "at least one active tenant role is required for a login account".into(),
    }
}

pub(crate) fn require_nonempty_role_assignment(role_ids: &[Uuid]) -> KabiPayResult<()> {
    if role_ids.is_empty() {
        return Err(active_login_role_required());
    }
    Ok(())
}

pub(crate) async fn validated_active_role_ids<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    role_ids: &[Uuid],
) -> KabiPayResult<Vec<Uuid>> {
    require_nonempty_role_assignment(role_ids)?;
    let mut seen = HashSet::with_capacity(role_ids.len());
    let unique = role_ids
        .iter()
        .copied()
        .filter(|role_id| seen.insert(*role_id))
        .collect::<Vec<_>>();
    let rows = role::Entity::find()
        .filter(role::Column::Id.is_in(unique.clone()))
        .filter(role::Column::TenantId.eq(tenant_id))
        .filter(role::Column::IsDeleted.eq(false))
        .lock_shared()
        .all(db)
        .await?;
    let valid_ids = rows
        .into_iter()
        .filter(|row| row.tenant_id == tenant_id && !row.is_deleted)
        .map(|row| row.id)
        .collect::<HashSet<_>>();
    if let Some(role_id) = unique.iter().find(|role_id| !valid_ids.contains(role_id)) {
        return Err(KabiPayError::NotFound {
            entity: "role",
            id: role_id.to_string(),
        });
    }
    Ok(unique)
}

pub(crate) async fn require_user_active_tenant_role<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<()> {
    let assigned_role_ids = user_role::Entity::find()
        .filter(user_role::Column::UserId.eq(user_id))
        .all(db)
        .await?
        .into_iter()
        .map(|assignment| assignment.role_id)
        .collect::<Vec<_>>();
    if assigned_role_ids.is_empty() {
        return Err(active_login_role_required());
    }
    let has_active_tenant_role = role::Entity::find()
        .filter(role::Column::Id.is_in(assigned_role_ids))
        .filter(role::Column::TenantId.eq(tenant_id))
        .filter(role::Column::IsDeleted.eq(false))
        .lock_shared()
        .one(db)
        .await?
        .is_some_and(|row| row.tenant_id == tenant_id && !row.is_deleted);
    if !has_active_tenant_role {
        return Err(active_login_role_required());
    }
    Ok(())
}

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
    set_role_permissions_for_actor(db,tenant_id,role_id,permission_ids,None).await
}
pub async fn set_role_permissions_for_actor(db:&DatabaseConnection,tenant_id:Uuid,role_id:Uuid,permission_ids:Vec<Uuid>,actor:Option<Uuid>)->KabiPayResult<()> {
    access_configuration::replace_access(db, tenant_id, role_id, Some(permission_ids), None, actor).await
}

pub async fn set_user_roles(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
    role_ids: Vec<Uuid>,
) -> KabiPayResult<()> {
    set_user_roles_for_actor(db,tenant_id,user_id,role_ids,None).await
}
pub async fn set_user_roles_for_actor(db: &DatabaseConnection, tenant_id: Uuid, user_id: Uuid, role_ids: Vec<Uuid>, actor: Option<Uuid>) -> KabiPayResult<()> {
    require_nonempty_role_assignment(&role_ids)?;
    let txn = db.begin().await?;
    let replacement = async {
        if let Some(actor_id) = actor {
            require_fresh_direct_rbac_admin(&txn, tenant_id, actor_id).await?;
        }
        guard_account_change(&txn,tenant_id,user_id,actor==Some(user_id),Some(&role_ids),None).await?;
        replace_user_roles_in_transaction(&txn, tenant_id, user_id, &role_ids).await
    }.await;
    if let Err(error) = replacement {
        txn.rollback().await?;
        return Err(error);
    }
    txn.commit().await?;
    Ok(())
}

pub(crate) async fn replace_user_roles_in_transaction(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    user_id: Uuid,
    role_ids: &[Uuid],
) -> KabiPayResult<()> {
    user::Entity::find_by_id(user_id)
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsDeleted.eq(false))
        .lock_exclusive()
        .one(txn)
        .await?
        .filter(|row| row.tenant_id == tenant_id && !row.is_deleted)
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "user",
            id: user_id.to_string(),
        })?;
    let role_ids = validated_active_role_ids(txn, tenant_id, role_ids).await?;
    user_role::Entity::delete_many()
        .filter(user_role::Column::UserId.eq(user_id))
        .exec(txn)
        .await?;
    let now = Utc::now();
    for role_id in role_ids {
        user_role::ActiveModel {
            user_id: Set(user_id),
            role_id: Set(role_id),
            assigned_at: Set(now),
        }
        .insert(txn)
        .await?;
    }
    user_session::Entity::delete_many()
        .filter(user_session::Column::UserId.eq(user_id))
        .exec(txn)
        .await?;
    Ok(())
}

/// Serialize account authority changes and preserve an effective local administrator.
pub(crate) async fn guard_account_change(
    txn: &DatabaseTransaction, tenant: Uuid, target: Uuid, same_actor: bool,
    replacement_roles: Option<&[Uuid]>, active: Option<bool>,
) -> KabiPayResult<()> {
    use kabipay_organization_tenant::statement;
    txn.execute(statement("SELECT pg_advisory_xact_lock(hashtextextended($1,0))", vec![format!("company-account-admin:{tenant}").into()])).await?;
    let row = txn.query_one(statement("SELECT EXISTS(SELECT 1 FROM user_role ur JOIN role r ON r.id=ur.role_id AND r.tenant_id=$1 AND NOT r.is_deleted JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id AND p.resource='role' AND p.action='manage' JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=$1 AND ps.resource='role' AND ps.action='manage' AND ps.scope_type='ALL' WHERE ur.user_id=$2) AS admin", vec![tenant.into(), target.into()])).await?.ok_or_else(|| KabiPayError::Internal("Account authority unavailable".into()))?;
    let was_admin: bool = row.try_get("", "admin")?;
    let retains_admin = if let Some(roles) = replacement_roles {
        txn.query_one(statement("SELECT EXISTS(SELECT 1 FROM role r JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id AND p.resource='role' AND p.action='manage' JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=$1 AND ps.resource='role' AND ps.action='manage' AND ps.scope_type='ALL' WHERE r.tenant_id=$1 AND NOT r.is_deleted AND r.id IN (SELECT value::uuid FROM jsonb_array_elements_text($2::jsonb))) AS admin", vec![tenant.into(), serde_json::json!(roles).into()])).await?.ok_or_else(|| KabiPayError::Internal("Account roles unavailable".into()))?.try_get::<bool>("", "admin")?
    } else { was_admin };
    enforce_self_admin_guard(was_admin, retains_admin && active != Some(false), same_actor)?;
    if was_admin && (!retains_admin || active == Some(false)) {
        let count = txn.query_one(statement("SELECT count(DISTINCT u.id) AS n FROM \"user\" u JOIN user_role ur ON ur.user_id=u.id JOIN role r ON r.id=ur.role_id AND r.tenant_id=$1 AND NOT r.is_deleted JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id AND p.resource='role' AND p.action='manage' JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=$1 AND ps.resource='role' AND ps.action='manage' AND ps.scope_type='ALL' WHERE u.tenant_id=$1 AND u.id<>$2 AND u.is_active AND NOT u.is_deleted AND NOT u.must_change_password AND (SELECT count(*)=0 OR (count(*)=1 AND bool_and(NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE'))) FROM employee e WHERE e.tenant_id=$1 AND e.user_id=u.id)", vec![tenant.into(),target.into()])).await?.ok_or_else(||KabiPayError::Internal("Administrator count unavailable".into()))?.try_get::<i64>("","n")?;
        if count == 0 { return Err(KabiPayError::Forbidden("The last active company administrator cannot be removed or disabled".into())); }
    }
    Ok(())
}

pub(crate) fn enforce_self_admin_guard(was_admin: bool, retains_admin: bool, same_actor: bool) -> KabiPayResult<()> {
    if same_actor && was_admin && !retains_admin { return Err(KabiPayError::Forbidden("An administrator cannot remove their own administration access".into())); }
    Ok(())
}

/// Account edits share tenant validation and session invalidation independently of transport.
pub(crate) async fn update_account_in_transaction(tx: &DatabaseTransaction,tenant:Uuid,id:Uuid,username:Option<String>,email:Option<String>)->KabiPayResult<()> {
    use kabipay_organization_tenant::{statement,unavailable};
    if let Some(value)=&username {if value.trim().is_empty() || value.len()>200{return Err(KabiPayError::Validation("Invalid username".into()));}}
    if let Some(value)=&email {if !value.is_empty() && (!value.contains('@') || value.len()>254){return Err(KabiPayError::Validation("Invalid email".into()));}}
    let username=username.as_deref().map(super::employee_service::normalize_username).transpose()?;
    let email=email.map(|value|super::employee_service::normalize_email(Some(value)).unwrap_or_default());
    let result=tx.execute(statement("UPDATE \"user\" SET username=COALESCE($3,username),email=CASE WHEN $4::text IS NULL THEN email ELSE NULLIF($4,'') END,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted",vec![tenant.into(),id.into(),username.into(),email.into()])).await.map_err(super::employee_service::map_employee_db_error)?;
    if result.rows_affected()!=1{return Err(unavailable());}
    user_session::Entity::delete_many().filter(user_session::Column::UserId.eq(id)).exec(tx).await?;
    Ok(())
}

pub(crate) async fn set_account_access_in_transaction(tx:&DatabaseTransaction,tenant:Uuid,id:Uuid,active:bool,same_actor:bool)->KabiPayResult<()> {
    use kabipay_organization_tenant::{statement,unavailable};
    guard_account_change(tx,tenant,id,same_actor,None,Some(active)).await?;
    if active {
        require_user_active_tenant_role(tx,tenant,id).await?;
        if tx.query_one(statement("SELECT 1 AS n WHERE NOT (SELECT count(*)=0 OR (count(*)=1 AND bool_and(NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE'))) FROM employee WHERE tenant_id=$1 AND user_id=$2)",vec![tenant.into(),id.into()])).await?.is_some(){return Err(KabiPayError::Validation("Employment status does not permit account activation".into()));}
    }
    let result=tx.execute(statement("UPDATE \"user\" SET is_active=$3,updated_at=now() WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted",vec![tenant.into(),id.into(),active.into()])).await?;
    if result.rows_affected()!=1{return Err(unavailable());}
    user_session::Entity::delete_many().filter(user_session::Column::UserId.eq(id)).exec(tx).await?;
    Ok(())
}

#[cfg(test)]
mod administration_guard_tests {
    use super::*;
    #[test]
    fn self_admin_removal_is_denied_but_preserved_admin_is_allowed() {
        assert!(enforce_self_admin_guard(true,false,true).is_err());
        assert!(enforce_self_admin_guard(true,true,true).is_ok());
        assert!(enforce_self_admin_guard(true,false,false).is_ok());
    }
}

pub async fn set_role_permission_scopes(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_id: Uuid,
    scopes: Vec<(String, String, String)>,
) -> KabiPayResult<()> {
    set_role_permission_scopes_for_actor(db,tenant_id,role_id,scopes,None).await
}
pub async fn set_role_permission_scopes_for_actor(db:&DatabaseConnection,tenant_id:Uuid,role_id:Uuid,scopes:Vec<(String,String,String)>,actor:Option<Uuid>)->KabiPayResult<()> {
    access_configuration::replace_access(db, tenant_id, role_id, None, Some(scopes), actor).await
}

async fn admin_authority_state(tx:&DatabaseTransaction,tenant:Uuid,actor:Option<Uuid>)->KabiPayResult<(i64,bool)> {
    let row=tx.query_one(kabipay_organization_tenant::statement("SELECT count(DISTINCT u.id) AS n,COALESCE(bool_or(u.id=$2),false) AS own FROM \"user\" u JOIN user_role ur ON ur.user_id=u.id JOIN role r ON r.id=ur.role_id AND r.tenant_id=$1 AND NOT r.is_deleted JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id AND p.resource='role' AND p.action='manage' JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=$1 AND ps.resource='role' AND ps.action='manage' AND ps.scope_type='ALL' WHERE u.tenant_id=$1 AND u.is_active AND NOT u.is_deleted AND NOT u.must_change_password AND (SELECT count(*)=0 OR (count(*)=1 AND bool_and(NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE'))) FROM employee e WHERE e.tenant_id=$1 AND e.user_id=u.id)",vec![tenant.into(),actor.into()])).await?.ok_or_else(||KabiPayError::Internal("Administrator authority unavailable".into()))?;
    Ok((row.try_get("","n")?,row.try_get("","own")?))
}

async fn begin_role_authority_change(tx:&DatabaseTransaction,tenant:Uuid,actor:Option<Uuid>)->KabiPayResult<(i64,bool)> {
    tx.execute(kabipay_organization_tenant::statement("SELECT pg_advisory_xact_lock(hashtextextended($1,0))",vec![format!("company-account-admin:{tenant}").into()])).await?;
    let state = admin_authority_state(tx,tenant,actor).await?;
    if actor.is_some() && !state.1 {
        return Err(KabiPayError::Forbidden("Current company administration authority is required".into()));
    }
    Ok(state)
}

/// Direct account administration must refresh authority while holding the same
/// transaction lock used by role revocation. Organization/operator callers use
/// their separately verified authority rather than this direct-user admission.
pub(crate) async fn require_fresh_direct_rbac_admin(tx:&DatabaseTransaction,tenant:Uuid,actor:Uuid)->KabiPayResult<()> {
    begin_role_authority_change(tx, tenant, Some(actor)).await.map(|_| ())
}
async fn finish_role_authority_change(tx:&DatabaseTransaction,tenant:Uuid,actor:Option<Uuid>,before:(i64,bool))->KabiPayResult<()> {
    let after=admin_authority_state(tx,tenant,actor).await?;
    if before.0>0 && after.0==0{return Err(KabiPayError::Forbidden("Role configuration cannot remove the last active company administrator".into()));}
    enforce_self_admin_guard(before.1,after.1,actor.is_some())
}

#[cfg(test)]
mod role_lifecycle_tests {
    use super::*;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
    };
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ScriptedProxy {
        query_results: Mutex<VecDeque<Result<Vec<ProxyRow>, DbErr>>>,
        events: Arc<Mutex<Vec<String>>>,
        fail_session_revocation: bool,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for ScriptedProxy {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.events
                .lock()
                .expect("event recorder")
                .push(format!("QUERY {statement}"));
            // Existing lifecycle fixtures represent an ordinary employee role.
            // Administration authority itself is exercised by dedicated source/PG cases.
            if statement.sql.contains(" AS n,COALESCE") {
                return Ok(vec![ProxyRow::new(BTreeMap::from([
                    ("n".into(), 1_i64.into()), ("own".into(), false.into()),
                ]))]);
            }
            if statement.sql.contains(" AS admin") {
                return Ok(vec![ProxyRow::new(BTreeMap::from([("admin".into(), false.into())]))]);
            }
            self.query_results
                .lock()
                .expect("query script")
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
            let rendered = format!("{statement}");
            self.events
                .lock()
                .expect("event recorder")
                .push(format!("EXECUTE {rendered}"));
            if self.fail_session_revocation && rendered.contains("user_session") {
                return Err(DbErr::Custom("session revocation failed".into()));
            }
            Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
        }

        async fn begin(&self) {
            self.events.lock().expect("event recorder").push("BEGIN".into());
        }

        async fn commit(&self) {
            self.events
                .lock()
                .expect("event recorder")
                .push("COMMIT".into());
        }

        async fn rollback(&self) {
            self.events
                .lock()
                .expect("event recorder")
                .push("ROLLBACK".into());
        }
    }

    async fn scripted_connection(
        query_results: Vec<Result<Vec<ProxyRow>, DbErr>>,
        fail_session_revocation: bool,
    ) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(ScriptedProxy {
                query_results: Mutex::new(query_results.into()),
                events: Arc::clone(&events),
                fail_session_revocation,
            })),
        )
        .await
        .expect("PostgreSQL proxy connection");
        (db, events)
    }

    fn user_row(user_id: Uuid, tenant_id: Uuid, is_active: bool) -> ProxyRow {
        let now = Utc::now();
        ProxyRow::new(BTreeMap::from([
            ("id".into(), user_id.into()),
            ("tenant_id".into(), tenant_id.into()),
            ("username".into(), "role-test-user".to_string().into()),
            ("email".into(), Option::<String>::None.into()),
            ("password_hash".into(), "hash".to_string().into()),
            ("must_change_password".into(), false.into()),
            ("is_active".into(), is_active.into()),
            ("mfa_enabled".into(), false.into()),
            ("mfa_secret".into(), Option::<String>::None.into()),
            ("last_login_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("is_deleted".into(), false.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]))
    }

    fn role_row(role_id: Uuid, tenant_id: Uuid, is_deleted: bool) -> ProxyRow {
        let now = Utc::now();
        ProxyRow::new(BTreeMap::from([
            ("id".into(), role_id.into()),
            ("tenant_id".into(), tenant_id.into()),
            ("name".into(), "EMPLOYEE".to_string().into()),
            ("description".into(), Option::<String>::None.into()),
            ("is_system_role".into(), true.into()),
            ("is_deleted".into(), is_deleted.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]))
    }

    fn user_role_row(user_id: Uuid, role_id: Uuid) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("user_id".into(), user_id.into()),
            ("role_id".into(), role_id.into()),
            ("assigned_at".into(), Utc::now().into()),
        ]))
    }

    #[tokio::test]
    async fn revoked_direct_actor_cannot_restore_authority_by_assigning_roles() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) = scripted_connection(vec![
            Ok(vec![user_row(user_id, tenant_id, true)]),
            Ok(vec![role_row(role_id, tenant_id, false)]),
            Ok(vec![user_role_row(user_id, role_id)]),
        ], false).await;
        let error = set_user_roles_for_actor(&db, tenant_id, user_id, vec![role_id], Some(user_id))
            .await.expect_err("revoked administrator cannot assign roles through an old bearer");
        assert_eq!(error.code(), "FORBIDDEN");
        let events = events.lock().expect("event recorder");
        assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
        assert!(!events.iter().any(|event| event.contains("DELETE FROM") || event == "COMMIT"));
    }

    #[tokio::test]
    async fn set_user_roles_rejects_empty_assignment_before_database_access() {
        let error = set_user_roles(
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Vec::new(),
        )
        .await
        .expect_err("an ordinary role replacement must never clear all roles");

        assert_eq!(error.code(), "ACTIVE_LOGIN_ROLE_REQUIRED");
    }

    #[tokio::test]
    async fn wrong_tenant_role_is_rejected_inside_the_replacement_transaction() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) = scripted_connection(
            vec![
                Ok(vec![user_row(user_id, tenant_id, true)]),
                Ok(vec![role_row(role_id, Uuid::new_v4(), false)]),
            ],
            false,
        )
        .await;

        let error = set_user_roles(&db, tenant_id, user_id, vec![role_id])
            .await
            .expect_err("cross-tenant role must be rejected");

        assert_eq!(error.code(), "NOT_FOUND");
        let events = events.lock().expect("event recorder");
        assert_eq!(events.first().map(String::as_str), Some("BEGIN"));
        assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
        assert!(!events.iter().any(|event| event.contains("DELETE FROM \"user_role\"")));
    }

    #[tokio::test]
    async fn deleted_role_is_rejected_inside_the_replacement_transaction() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) = scripted_connection(
            vec![
                Ok(vec![user_row(user_id, tenant_id, true)]),
                Ok(vec![role_row(role_id, tenant_id, true)]),
            ],
            false,
        )
        .await;

        let error = set_user_roles(&db, tenant_id, user_id, vec![role_id])
            .await
            .expect_err("deleted role must be rejected");

        assert_eq!(error.code(), "NOT_FOUND");
        let events = events.lock().expect("event recorder");
        assert_eq!(events.first().map(String::as_str), Some("BEGIN"));
        assert_eq!(events.last().map(String::as_str), Some("ROLLBACK"));
        assert!(!events.iter().any(|event| event.contains("DELETE FROM \"user_role\"")));
    }

    #[tokio::test]
    async fn role_replacement_revokes_sessions_before_the_same_transaction_commits() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) = scripted_connection(
            vec![
                Ok(vec![user_row(user_id, tenant_id, true)]),
                Ok(vec![role_row(role_id, tenant_id, false)]),
                Ok(vec![user_role_row(user_id, role_id)]),
            ],
            false,
        )
        .await;

        set_user_roles(&db, tenant_id, user_id, vec![role_id])
            .await
            .expect("valid replacement should commit");

        let events = events.lock().expect("event recorder");
        let begin = events.iter().position(|event| event == "BEGIN").expect("begin");
        let revoke = events
            .iter()
            .position(|event| event.contains("DELETE FROM \"user_session\""))
            .expect("session revocation");
        let commit = events
            .iter()
            .position(|event| event == "COMMIT")
            .expect("commit");
        assert!(begin < revoke && revoke < commit, "events={events:?}");
    }

    #[tokio::test]
    async fn session_revocation_failure_rolls_back_role_replacement() {
        let tenant_id = Uuid::new_v4();
        let user_id = Uuid::new_v4();
        let role_id = Uuid::new_v4();
        let (db, events) = scripted_connection(
            vec![
                Ok(vec![user_row(user_id, tenant_id, true)]),
                Ok(vec![role_row(role_id, tenant_id, false)]),
                Ok(vec![user_role_row(user_id, role_id)]),
            ],
            true,
        )
        .await;

        let error = set_user_roles(&db, tenant_id, user_id, vec![role_id])
            .await
            .expect_err("session revocation failure must abort the role change");

        assert_eq!(error.code(), "DATABASE_ERROR");
        let events = events.lock().expect("event recorder");
        assert!(events.iter().any(|event| event == "ROLLBACK"), "events={events:?}");
        assert!(!events.iter().any(|event| event == "COMMIT"), "events={events:?}");
    }
}
