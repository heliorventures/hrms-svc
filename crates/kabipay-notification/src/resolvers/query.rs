//! Root query resolvers for kabipay-notification.

use async_graphql::{Context, Enum, ID, Object, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use kabipay_common::{
    subgraph::{
        require_client_claims, require_tenant_id, tenant_db, try_client_employee_dept_and_location,
    },
    KabiPayError,
};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::resolvers::types::{
    AnnouncementAttachmentDto, AnnouncementDto, NotificationDto, NotificationPreferencesGql,
};
use crate::services::notification_service;

pub struct QueryRoot;

const MAX_INLINE_ANNOUNCEMENT_ATTACHMENT_BYTES: usize = 6 * 1024 * 1024;

#[derive(Copy, Clone, Eq, PartialEq, Enum)]
pub enum AnnouncementAttachmentKind {
    Image,
    Document,
}

fn attachment_storage_id(
    image_id: Option<Uuid>,
    document_id: Option<Uuid>,
    kind: AnnouncementAttachmentKind,
) -> Option<Uuid> {
    match kind {
        AnnouncementAttachmentKind::Image => image_id,
        AnnouncementAttachmentKind::Document => document_id,
    }
}

fn announcement_is_currently_visible(
    publish_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
    now: DateTime<Utc>,
) -> bool {
    publish_at.is_none_or(|value| value <= now) && expires_at.is_none_or(|value| value > now)
}

#[cfg(test)]
mod tests {
    use super::{announcement_is_currently_visible, attachment_storage_id, AnnouncementAttachmentKind};
    use chrono::{Duration, Utc};
    use uuid::Uuid;

    #[test]
    fn attachment_kind_selects_only_the_requested_storage_id() {
        let image_id = Uuid::new_v4();
        let document_id = Uuid::new_v4();

        assert_eq!(
            attachment_storage_id(Some(image_id), Some(document_id), AnnouncementAttachmentKind::Image),
            Some(image_id)
        );
        assert_eq!(
            attachment_storage_id(
                Some(image_id),
                Some(document_id),
                AnnouncementAttachmentKind::Document,
            ),
            Some(document_id)
        );
    }

    #[test]
    fn scheduled_or_expired_announcement_attachment_is_not_currently_visible() {
        let now = Utc::now();
        assert!(!announcement_is_currently_visible(
            Some(now + Duration::minutes(1)),
            None,
            now,
        ));
        assert!(!announcement_is_currently_visible(
            None,
            Some(now),
            now,
        ));
        assert!(announcement_is_currently_visible(
            Some(now - Duration::minutes(1)),
            Some(now + Duration::minutes(1)),
            now,
        ));
    }
}

#[Object]
impl QueryRoot {
    async fn notification_health(&self) -> &'static str {
        "ok"
    }

    async fn announcements(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<AnnouncementDto>> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let bypass = claims.can_manage_notifications();
        let (viewer_dept, viewer_loc) = if bypass {
            (None, None)
        } else {
            match try_client_employee_dept_and_location(&db, tenant_id, claims).await {
                Ok(Some((d, l))) => (d, l),
                Ok(None) => (None, None),
                Err(e) => return Err(e.into_graphql()),
            }
        };
        let prefs = crate::services::notification_preference::load_notification_prefs(
            &db,
            tenant_id,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        if !bypass && !prefs.announcements_enabled {
            return Ok(vec![]);
        }
        let rows = notification_service::list_announcements_visible(
            &db,
            tenant_id,
            limit,
            bypass,
            viewer_dept,
            viewer_loc,
            &claims.roles,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AnnouncementDto::from).collect())
    }

    /// Load one private announcement attachment after validating the owning announcement's
    /// tenant, publication window, audience and notification preference.
    async fn announcement_attachment(
        &self,
        ctx: &Context<'_>,
        announcement_id: ID,
        kind: AnnouncementAttachmentKind,
    ) -> Result<AnnouncementAttachmentDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let announcement_id = Uuid::parse_str(announcement_id.as_str()).map_err(|error| {
            KabiPayError::Validation(format!("announcementId: {error}")).into_graphql()
        })?;
        let row = notification_service::get_announcement(&db, tenant_id, announcement_id)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "announcement",
                    id: announcement_id.to_string(),
                }
                .into_graphql()
            })?;

        let bypass = claims.can_manage_notifications();
        if !bypass {
            let preferences = crate::services::notification_preference::load_notification_prefs(
                &db,
                tenant_id,
                claims.sub,
            )
            .await
            .map_err(KabiPayError::into_graphql)?;
            if !preferences.announcements_enabled
                || !announcement_is_currently_visible(
                    row.publish_at.clone(),
                    row.expires_at.clone(),
                    Utc::now(),
                )
            {
                return Err(KabiPayError::Forbidden(
                    "announcement attachment is not available to this user".into(),
                )
                .into_graphql());
            }
            let (viewer_department, viewer_location) =
                match try_client_employee_dept_and_location(&db, tenant_id, claims).await {
                    Ok(Some((department, location))) => (department, location),
                    Ok(None) => (None, None),
                    Err(error) => return Err(error.into_graphql()),
                };
            if !notification_service::announcement_visible_to_viewer(
                &row,
                false,
                viewer_department,
                viewer_location,
                &claims.roles,
            ) {
                return Err(KabiPayError::Forbidden(
                    "announcement attachment is not available to this user".into(),
                )
                .into_graphql());
            }
        }

        let file_id = attachment_storage_id(
            row.image_file_storage_id,
            row.document_file_storage_id,
            kind,
        )
        .ok_or_else(|| {
            KabiPayError::NotFound {
                entity: "announcementAttachment",
                id: announcement_id.to_string(),
            }
            .into_graphql()
        })?;
        let stored_file = file_storage::Entity::find_by_id(file_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(|error: sea_orm::DbErr| KabiPayError::from(error).into_graphql())?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "announcementAttachment",
                    id: announcement_id.to_string(),
                }
                .into_graphql()
            })?;
        if stored_file
            .file_size_bytes
            .is_some_and(|size| size > MAX_INLINE_ANNOUNCEMENT_ATTACHMENT_BYTES as i64)
        {
            return Err(KabiPayError::Validation(
                "announcement attachment is too large to return inline".into(),
            )
            .into_graphql());
        }
        let bytes = crate::services::announcement_storage::read_blob(&stored_file)
            .await
            .map_err(|error| match error {
                KabiPayError::NotFound { .. } => KabiPayError::NotFound {
                    entity: "announcementAttachment",
                    id: announcement_id.to_string(),
                }
                .into_graphql(),
                other => other.into_graphql(),
            })?;
        if bytes.len() > MAX_INLINE_ANNOUNCEMENT_ATTACHMENT_BYTES {
            return Err(KabiPayError::Validation(
                "announcement attachment is too large to return inline".into(),
            )
            .into_graphql());
        }

        Ok(AnnouncementAttachmentDto {
            file_name: stored_file
                .original_filename
                .filter(|name| !name.trim().is_empty())
                .unwrap_or_else(|| "attachment".into()),
            mime_type: stored_file
                .mime_type
                .filter(|mime| !mime.trim().is_empty())
                .unwrap_or_else(|| "application/octet-stream".into()),
            file_size_bytes: stored_file
                .file_size_bytes
                .and_then(|size| i32::try_from(size).ok()),
            content_base64: STANDARD.encode(bytes),
        })
    }

    /// Admin / HR: all recent announcements including scheduled or expired (for management UI).
    async fn admin_announcements(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AnnouncementDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_notifications() {
            return Err(
                KabiPayError::Forbidden("notification:manage or equivalent role required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = notification_service::list_announcements_admin(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AnnouncementDto::from).collect())
    }

    async fn notifications(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<NotificationDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows =
            notification_service::list_notifications_for_user(&db, tenant_id, claims.sub, limit)
                .await
                .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(NotificationDto::from).collect())
    }

    /// Admin / HR: recent in-app notifications tenant-wide (for support / auditing).
    async fn admin_notifications(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<NotificationDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_notifications() {
            return Err(
                KabiPayError::Forbidden("notification:manage or equivalent role required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = notification_service::list_notifications(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(NotificationDto::from).collect())
    }

    async fn unread_notification_count(&self, ctx: &Context<'_>) -> Result<u64> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        notification_service::count_unread_for_user(&db, tenant_id, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// Current user’s in-app visibility preferences (announcement bulletin + per-topic mutes).
    async fn my_notification_preferences(&self, ctx: &Context<'_>) -> Result<NotificationPreferencesGql> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let p = crate::services::notification_preference::load_notification_prefs(
            &db,
            tenant_id,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(NotificationPreferencesGql::from_prefs(p))
    }
}
