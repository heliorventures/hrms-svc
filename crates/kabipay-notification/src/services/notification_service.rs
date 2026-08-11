//! Tenant-scoped SeaORM queries and commands for in-app notifications and announcements.
//! **Outbound email/SMS** is not implemented here; see the crate **`README.md`** for the roadmap.

use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0005_auth_rbac::{role, user, user_role};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0027_communication_audit::{announcement, notification};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use std::collections::HashSet;
use uuid::Uuid;

use crate::services::notification_preference;

pub async fn list_announcements_visible(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    bypass_audience: bool,
    viewer_dept: Option<Uuid>,
    viewer_loc: Option<Uuid>,
    viewer_roles: &[String],
) -> KabiPayResult<Vec<announcement::Model>> {
    let limit = limit.clamp(1, 200);
    let now = Utc::now();
    let rows: Vec<announcement::Model> = announcement::Entity::find()
        .filter(announcement::Column::TenantId.eq(tenant_id))
        .filter(
            Condition::any()
                .add(announcement::Column::PublishAt.is_null())
                .add(announcement::Column::PublishAt.lte(now)),
        )
        .filter(
            Condition::any()
                .add(announcement::Column::ExpiresAt.is_null())
                .add(announcement::Column::ExpiresAt.gt(now)),
        )
        .order_by_desc(announcement::Column::CreatedAt)
        .limit(500)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let out: Vec<_> = rows
        .into_iter()
        .filter(|m| {
            announcement_visible_to_viewer(m, bypass_audience, viewer_dept, viewer_loc, viewer_roles)
        })
        .take(limit as usize)
        .collect();
    Ok(out)
}

pub fn announcement_visible_to_viewer(
    m: &announcement::Model,
    bypass_audience: bool,
    viewer_dept: Option<Uuid>,
    viewer_loc: Option<Uuid>,
    viewer_roles: &[String],
) -> bool {
    if bypass_audience {
        return true;
    }
    if let Some(ref ta) = m.target_audience {
        // `String::trim` on `&String` — avoid `as_str()` here (can confuse inference when deps fail mid-build).
        let t: &str = ta.trim();
        let upper = t.to_ascii_uppercase();
        if upper.starts_with("ROLE:") {
            let need = t[5..].trim().to_ascii_uppercase();
            if !viewer_roles
                .iter()
                .any(|r| r.trim().to_ascii_uppercase() == need)
            {
                return false;
            }
        }
    }
    if let Some(did) = m.target_department_id {
        if viewer_dept != Some(did) {
            return false;
        }
    }
    if let Some(lid) = m.target_location_id {
        if viewer_loc != Some(lid) {
            return false;
        }
    }
    true
}

fn announcement_role_target(m: &announcement::Model) -> Option<String> {
    let value = m.target_audience.as_ref()?.trim();
    let upper = value.to_ascii_uppercase();
    upper
        .strip_prefix("ROLE:")
        .map(|role| role.trim().to_string())
        .filter(|role| !role.is_empty())
}

async fn active_user_ids(db: &DatabaseConnection, tenant_id: Uuid) -> KabiPayResult<HashSet<Uuid>> {
    let rows = user::Entity::find()
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::IsDeleted.eq(false))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows.into_iter().map(|row| row.id).collect())
}

async fn role_target_user_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    role_code: &str,
) -> KabiPayResult<HashSet<Uuid>> {
    let Some(role_row) = role::Entity::find()
        .filter(role::Column::TenantId.eq(tenant_id))
        .filter(role::Column::Name.eq(role_code))
        .filter(role::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
    else {
        return Ok(HashSet::new());
    };
    let rows = user_role::Entity::find()
        .filter(user_role::Column::RoleId.eq(role_row.id))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows.into_iter().map(|row| row.user_id).collect())
}

async fn employee_target_user_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    department_id: Option<Uuid>,
    location_id: Option<Uuid>,
) -> KabiPayResult<HashSet<Uuid>> {
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::UserId.is_not_null());
    if let Some(department_id) = department_id {
        query = query.filter(employee::Column::DepartmentId.eq(Some(department_id)));
    }
    if let Some(location_id) = location_id {
        query = query.filter(employee::Column::LocationId.eq(Some(location_id)));
    }
    let rows = query.all(db).await.map_err(KabiPayError::from)?;
    Ok(rows.into_iter().filter_map(|row| row.user_id).collect())
}

async fn announcement_recipient_user_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    m: &announcement::Model,
) -> KabiPayResult<Vec<Uuid>> {
    let mut recipients = active_user_ids(db, tenant_id).await?;
    if let Some(role_code) = announcement_role_target(m) {
        let role_users = role_target_user_ids(db, tenant_id, &role_code).await?;
        recipients.retain(|user_id| role_users.contains(user_id));
    }
    if m.target_department_id.is_some() || m.target_location_id.is_some() {
        let employee_users =
            employee_target_user_ids(db, tenant_id, m.target_department_id, m.target_location_id)
                .await?;
        recipients.retain(|user_id| employee_users.contains(user_id));
    }
    Ok(recipients.into_iter().collect())
}

/// Full list for admins (includes scheduled / expired rows; caller still scopes by tenant).
pub async fn list_announcements_admin(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<announcement::Model>> {
    let limit = limit.clamp(1, 300);
    announcement::Entity::find()
        .filter(announcement::Column::TenantId.eq(tenant_id))
        .order_by_desc(announcement::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn get_announcement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Uuid,
) -> KabiPayResult<Option<announcement::Model>> {
    announcement::Entity::find()
        .filter(announcement::Column::Id.eq(id))
        .filter(announcement::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_notifications_for_user(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<notification::Model>> {
    let limit = limit.clamp(1, 500);
    let prefs = notification_preference::load_notification_prefs(db, tenant_id, user_id).await?;
    if !prefs.in_app_enabled {
        return Ok(vec![]);
    }
    let fetch_limit = (limit * 4).min(500).max(limit);
    let rows: Vec<notification::Model> = notification::Entity::find()
        .filter(notification::Column::TenantId.eq(tenant_id))
        .filter(notification::Column::UserId.eq(user_id))
        .order_by_desc(notification::Column::CreatedAt)
        .limit(fetch_limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows
        .into_iter()
        .filter(|m| notification_preference::row_visible(m, &prefs))
        .take(limit as usize)
        .collect())
}

pub async fn count_unread_for_user(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<u64> {
    const CAP: u64 = 5000;
    let prefs = notification_preference::load_notification_prefs(db, tenant_id, user_id).await?;
    if !prefs.in_app_enabled {
        return Ok(0);
    }
    let rows: Vec<notification::Model> = notification::Entity::find()
        .filter(notification::Column::TenantId.eq(tenant_id))
        .filter(notification::Column::UserId.eq(user_id))
        .filter(notification::Column::IsRead.eq(false))
        .order_by_desc(notification::Column::CreatedAt)
        .limit(CAP)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let n = rows
        .iter()
        .filter(|m| notification_preference::row_visible(m, &prefs))
        .count() as u64;
    Ok(n)
}

#[allow(dead_code)]
pub async fn list_notifications(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<notification::Model>> {
    let limit = limit.clamp(1, 500);
    notification::Entity::find()
        .filter(notification::Column::TenantId.eq(tenant_id))
        .order_by_desc(notification::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub struct NewAnnouncement {
    pub title: String,
    pub body: Option<String>,
    pub target_audience: Option<String>,
    /// `employee_post` for normal user posts; `company` for HR / system style posts.
    pub post_source: String,
    pub image_file_storage_id: Option<Uuid>,
    pub document_file_storage_id: Option<Uuid>,
    pub target_department_id: Option<Uuid>,
    pub target_location_id: Option<Uuid>,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
}

pub async fn create_announcement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    created_by: Uuid,
    new: NewAnnouncement,
) -> KabiPayResult<announcement::Model> {
    let id = Uuid::new_v4();
    let now = Utc::now();
    let publish_at = new.publish_at.unwrap_or(now);
    let am = announcement::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        created_by: Set(Some(created_by)),
        title: Set(new.title),
        body: Set(new.body),
        target_audience: Set(new.target_audience),
        target_department_id: Set(new.target_department_id),
        target_location_id: Set(new.target_location_id),
        publish_at: Set(Some(publish_at)),
        expires_at: Set(new.expires_at),
        image_file_storage_id: Set(new.image_file_storage_id),
        document_file_storage_id: Set(new.document_file_storage_id),
        post_source: Set(new.post_source),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    let row = announcement::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("announcement missing after insert".into()))?;
    let visible_now = row.publish_at.map(|t| t <= now).unwrap_or(true)
        && row.expires_at.map(|t| t > now).unwrap_or(true);
    if visible_now {
        let recipients = announcement_recipient_user_ids(db, tenant_id, &row).await?;
        insert_notifications_for_users(
            db,
            tenant_id,
            recipients,
            Some("ANNOUNCEMENT".into()),
            Some(row.title.clone()),
            row.body.clone(),
            Some("/notifications".into()),
        )
        .await?;
    }
    Ok(row)
}

pub struct AnnouncementUpdate {
    pub title: Option<String>,
    pub body: Option<String>,
    /// When true, clears `target_audience` and ignores `target_audience`.
    pub clear_target_audience: bool,
    pub target_audience: Option<String>,
    pub target_department_id: Option<Option<Uuid>>,
    pub target_location_id: Option<Option<Uuid>>,
    pub publish_at: Option<Option<DateTime<Utc>>>,
    pub expires_at: Option<Option<DateTime<Utc>>>,
    pub image_file_storage_id: Option<Option<Uuid>>,
    pub document_file_storage_id: Option<Option<Uuid>>,
}

pub async fn update_announcement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    announcement_id: Uuid,
    patch: AnnouncementUpdate,
) -> KabiPayResult<announcement::Model> {
    let row = get_announcement(db, tenant_id, announcement_id)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "announcement",
            id: announcement_id.to_string(),
        })?;
    let mut am: announcement::ActiveModel = row.into();
    let now = Utc::now();
    if let Some(t) = patch.title {
        let tt = t.trim().to_string();
        if tt.is_empty() {
            return Err(KabiPayError::Validation("title must not be empty".into()));
        }
        am.title = Set(tt);
    }
    if let Some(b) = patch.body {
        let b = b.trim();
        am.body = Set(if b.is_empty() { None } else { Some(b.to_string()) });
    }
    if patch.clear_target_audience {
        am.target_audience = Set(None);
    } else if let Some(a) = patch.target_audience {
        let a = a.trim();
        am.target_audience = Set(if a.is_empty() { None } else { Some(a.to_string()) });
    }
    if let Some(d) = patch.target_department_id {
        am.target_department_id = Set(d);
    }
    if let Some(l) = patch.target_location_id {
        am.target_location_id = Set(l);
    }
    if let Some(p) = patch.publish_at {
        am.publish_at = Set(p);
    }
    if let Some(e) = patch.expires_at {
        am.expires_at = Set(e);
    }
    if let Some(i) = patch.image_file_storage_id {
        am.image_file_storage_id = Set(i);
    }
    if let Some(d) = patch.document_file_storage_id {
        am.document_file_storage_id = Set(d);
    }
    am.updated_at = Set(now);
    am.update(db).await.map_err(KabiPayError::from)?;
    announcement::Entity::find_by_id(announcement_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("announcement missing after update".into()))
}

pub async fn delete_announcement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    announcement_id: Uuid,
) -> KabiPayResult<u64> {
    let r = announcement::Entity::delete_many()
        .filter(announcement::Column::Id.eq(announcement_id))
        .filter(announcement::Column::TenantId.eq(tenant_id))
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    if r.rows_affected == 0 {
        return Err(KabiPayError::NotFound {
            entity: "announcement",
            id: announcement_id.to_string(),
        });
    }
    Ok(r.rows_affected)
}

pub async fn create_notifications_for_users(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_ids: Vec<Uuid>,
    kind: Option<String>,
    title: Option<String>,
    message: Option<String>,
    action_url: Option<String>,
) -> KabiPayResult<u64> {
    if user_ids.is_empty() {
        return Err(KabiPayError::Validation(
            "at least one user id is required".into(),
        ));
    }
    if user_ids.len() > 500 {
        return Err(KabiPayError::Validation(
            "too many recipients (max 500)".into(),
        ));
    }
    insert_notifications_for_users(db, tenant_id, user_ids, kind, title, message, action_url).await
}

async fn insert_notifications_for_users(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_ids: Vec<Uuid>,
    kind: Option<String>,
    title: Option<String>,
    message: Option<String>,
    action_url: Option<String>,
) -> KabiPayResult<u64> {
    if user_ids.is_empty() {
        return Ok(0);
    }
    let now = Utc::now();
    let mut n = 0u64;
    for uid in user_ids {
        let id = Uuid::new_v4();
        let am = notification::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            user_id: Set(uid),
            r#type: Set(kind.clone()),
            title: Set(title.clone()),
            message: Set(message.clone()),
            action_url: Set(action_url.clone()),
            is_read: Set(false),
            read_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await.map_err(KabiPayError::from)?;
        n += 1;
    }
    Ok(n)
}

pub struct NotificationPatch {
    pub kind: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub action_url: Option<String>,
}

pub async fn update_notification_admin(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    notification_id: Uuid,
    patch: NotificationPatch,
) -> KabiPayResult<notification::Model> {
    let row = notification::Entity::find()
        .filter(notification::Column::Id.eq(notification_id))
        .filter(notification::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "notification",
            id: notification_id.to_string(),
        })?;
    let mut am: notification::ActiveModel = row.into();
    if let Some(k) = patch.kind {
        am.r#type = Set(Some(k));
    }
    if let Some(t) = patch.title {
        am.title = Set(Some(t));
    }
    if let Some(m) = patch.message {
        am.message = Set(Some(m));
    }
    if let Some(u) = patch.action_url {
        am.action_url = Set(Some(u));
    }
    am.updated_at = Set(Utc::now());
    am.update(db).await.map_err(KabiPayError::from)?;
    notification::Entity::find_by_id(notification_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("notification missing after update".into()))
}

pub async fn delete_notification_admin(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    notification_id: Uuid,
) -> KabiPayResult<()> {
    let r = notification::Entity::delete_many()
        .filter(notification::Column::Id.eq(notification_id))
        .filter(notification::Column::TenantId.eq(tenant_id))
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    if r.rows_affected == 0 {
        return Err(KabiPayError::NotFound {
            entity: "notification",
            id: notification_id.to_string(),
        });
    }
    Ok(())
}

/// `true` when `file_id` is linked from an announcement in this tenant (for signed read URLs).
pub async fn announcement_links_file_storage(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    file_id: Uuid,
) -> KabiPayResult<bool> {
    let row = announcement::Entity::find()
        .filter(announcement::Column::TenantId.eq(tenant_id))
        .filter(
            Condition::any()
                .add(announcement::Column::ImageFileStorageId.eq(file_id))
                .add(announcement::Column::DocumentFileStorageId.eq(file_id)),
        )
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(row.is_some())
}

/// Mark a single row read; returns `NotFound` if wrong id/tenant or not owned by `user_id`.
pub async fn mark_read(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
    notification_id: Uuid,
) -> KabiPayResult<notification::Model> {
    let row = notification::Entity::find()
        .filter(notification::Column::Id.eq(notification_id))
        .filter(notification::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "notification",
            id: notification_id.to_string(),
        })?;
    if row.user_id != user_id {
        return Err(KabiPayError::Forbidden(
            "notification belongs to another user".into(),
        ));
    }
    if row.is_read {
        return Ok(row);
    }
    let mut am: notification::ActiveModel = row.into();
    am.is_read = Set(true);
    am.read_at = Set(Some(Utc::now()));
    am.updated_at = Set(Utc::now());
    am.update(db).await?;
    notification::Entity::find_by_id(notification_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("notification row missing after update".into()))
}

/// Sets `is_read` on unread rows that are **visible** under the user’s notification preferences.
pub async fn mark_all_read(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<u64> {
    let prefs = notification_preference::load_notification_prefs(db, tenant_id, user_id).await?;
    if !prefs.in_app_enabled {
        return Ok(0);
    }
    let rows: Vec<notification::Model> = notification::Entity::find()
        .filter(notification::Column::TenantId.eq(tenant_id))
        .filter(notification::Column::UserId.eq(user_id))
        .filter(notification::Column::IsRead.eq(false))
        .all(db)
        .await?;
    let mut n = 0u64;
    for row in rows {
        if !notification_preference::row_visible(&row, &prefs) {
            continue;
        }
        let mut am: notification::ActiveModel = row.into();
        am.is_read = Set(true);
        am.read_at = Set(Some(Utc::now()));
        am.updated_at = Set(Utc::now());
        am.update(db).await?;
        n += 1;
    }
    Ok(n)
}
