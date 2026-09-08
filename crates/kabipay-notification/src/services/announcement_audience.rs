use chrono::{
    DateTime,Utc
};
use uuid::Uuid;
use kabipay_db_entities::tenant::d0027_communication_audit::announcement;
use super::notification_service;
pub(crate) fn announcement_is_currently_visible(
publish_at: Option<DateTime<Utc>>,
expires_at: Option<DateTime<Utc>>,
now: DateTime<Utc>,
) -> bool {
    publish_at.is_none_or(|value| value <= now) && expires_at.is_none_or(|value| value > now)
}
pub(crate) fn announcement_available_to_reader(
row: &announcement::Model,
announcements_enabled: bool,
now: DateTime<Utc>,
viewer_department: Option<Uuid>,
viewer_location: Option<Uuid>,
viewer_roles: &[String],
) -> bool {
    announcements_enabled
    && announcement_is_currently_visible(row.publish_at, row.expires_at, now)
    && notification_service::announcement_visible_to_viewer(
    row,
    false,
    viewer_department,
    viewer_location,
    viewer_roles,
    )
}
use kabipay_common::{
    KabiPayError, KabiPayResult
};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::{
        user,role,user_role
    },d0007_employee_core::employee
};
use sea_orm::{
    ColumnTrait,ConnectionTrait,DatabaseConnection,DbBackend,EntityTrait,QueryFilter,Statement
};
pub struct Viewer {
    pub roles: Vec<String>, pub department: Option<Uuid>, pub location: Option<Uuid>
}
pub async fn current_viewer(db:&DatabaseConnection, tenant:Uuid, owner:Uuid) -> KabiPayResult<Viewer> {
    let account=user::Entity::find_by_id(owner).filter(user::Column::TenantId.eq(tenant)).one(db).await?.ok_or(KabiPayError::Unauthorised)?;
    if !account.is_active || account.is_deleted || account.must_change_password {
        return Err(KabiPayError::Unauthorised);
    }
    // A scope is effective only when its exact permission is granted by the same active role.
    let allowed=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres, r#"SELECT 1 AS allowed FROM user_role ur JOIN role r ON r.id=ur.role_id AND r.tenant_id=$1 AND NOT r.is_deleted JOIN role_permission rp ON rp.role_id=r.id JOIN permission p ON p.id=rp.permission_id JOIN permission_scope ps ON ps.role_id=r.id AND ps.tenant_id=$1 AND lower(ps.resource)=lower(p.resource) AND lower(ps.action)=lower(p.action) WHERE ur.user_id=$2 AND lower(p.resource)='notification' AND lower(p.action)='read' AND upper(ps.scope_type) IN ('ALL','TEAM','SELF') LIMIT 1"#,vec![tenant.into(),owner.into()])).await?;
    if allowed.is_none() {
        return Err(KabiPayError::Forbidden("notification:read permission required".into()));
    }
    let assignments=user_role::Entity::find().filter(user_role::Column::UserId.eq(owner)).all(db).await?;
    let roles=role::Entity::find().filter(role::Column::TenantId.eq(tenant)).filter(role::Column::IsDeleted.eq(false)).filter(role::Column::Id.is_in(assignments.into_iter().map(|r|r.role_id))).all(db).await?.into_iter().map(|r|r.name).collect();
    let profile=employee::Entity::find().filter(employee::Column::TenantId.eq(tenant)).filter(employee::Column::UserId.eq(owner)).filter(employee::Column::IsDeleted.eq(false)).one(db).await?;
    Ok(Viewer{
        roles,department:profile.as_ref().and_then(|p|p.department_id),location:profile.and_then(|p|p.location_id)
    })
}
pub async fn authorized_parent(db:&DatabaseConnection,tenant:Uuid,owner:Uuid,id:Uuid)->KabiPayResult<announcement::Model>{
    let viewer=current_viewer(db,tenant,owner).await?;
    let prefs=super::notification_preference::load_notification_prefs(db,tenant,owner).await?;
    let row=notification_service::get_announcement(db,tenant,id).await?.filter(|r|announcement_available_to_reader(r,prefs.announcements_enabled,Utc::now(),viewer.department,viewer.location,&viewer.roles));
    row.ok_or_else(||KabiPayError::NotFound{
        entity:"announcementVideo",id:"requested".into()
    })
}
