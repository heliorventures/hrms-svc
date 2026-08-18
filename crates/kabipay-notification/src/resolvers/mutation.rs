//! Write operations for notifications (read state) and public announcements.

use async_graphql::{Context, Object, Result, ID};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AnnouncementDto, CreateAnnouncementInput, CreateDirectNotificationsInput,
    NotificationDto, NotificationPreferencesGql, UpdateAnnouncementInput, UpdateNotificationAdminInput,
    UpdateNotificationPreferencesInput,
};
use crate::services::announcement_storage;
use crate::services::notification_preference;
use crate::services::notification_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn can_edit_announcement(claims: &kabipay_common::context::ClientClaims, row: &kabipay_db_entities::tenant::d0027_communication_audit::announcement::Model) -> bool {
    claims.can_manage_notifications()
        || (row.post_source == "employee_post" && row.created_by == Some(claims.sub))
}

fn merged_target_audience(
    freeform: Option<String>,
    role_code: Option<String>,
) -> Result<Option<String>> {
    let role_trimmed = role_code
        .as_ref()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if let Some(rc) = role_trimmed {
        return Ok(Some(format!("ROLE:{rc}")));
    }
    Ok(freeform)
}

async fn maybe_store_image(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    uploader: Option<Uuid>,
    input: &CreateAnnouncementInput,
) -> Result<Option<Uuid>> {
    if let Some(ref raw) = input.image_content_base64 {
        let s = raw.trim();
        if !s.is_empty() {
            let bytes = STANDARD.decode(s).map_err(|e| {
                KabiPayError::Validation(format!("imageContentBase64: invalid base64 ({e})")).into_graphql()
            })?;
            let fname = input
                .image_file_name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "image".into());
            return Ok(Some(
                announcement_storage::store_blob(
                    db,
                    tenant_id,
                    uploader,
                    fname,
                    input.image_mime_type.clone(),
                    bytes,
                )
                .await
                .map_err(KabiPayError::into_graphql)?,
            ));
        }
    }
    Ok(None)
}

async fn maybe_store_doc(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    uploader: Option<Uuid>,
    input: &CreateAnnouncementInput,
) -> Result<Option<Uuid>> {
    if let Some(ref raw) = input.document_content_base64 {
        let s = raw.trim();
        if !s.is_empty() {
            let bytes = STANDARD.decode(s).map_err(|e| {
                KabiPayError::Validation(format!("documentContentBase64: invalid base64 ({e})")).into_graphql()
            })?;
            let fname = input
                .document_file_name
                .as_ref()
                .map(|n| n.trim().to_string())
                .filter(|n| !n.is_empty())
                .unwrap_or_else(|| "attachment".into());
            return Ok(Some(
                announcement_storage::store_blob(
                    db,
                    tenant_id,
                    uploader,
                    fname,
                    input.document_mime_type.clone(),
                    bytes,
                )
                .await
                .map_err(KabiPayError::into_graphql)?,
            ));
        }
    }
    Ok(None)
}

async fn cleanup_attachment_ids(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    file_ids: impl IntoIterator<Item = Uuid>,
) {
    for file_id in file_ids {
        if let Err(error) =
            announcement_storage::delete_blob_if_unreferenced(db, tenant_id, file_id).await
        {
            tracing::warn!(
                tenant_id = %tenant_id,
                code = error.code(),
                "announcement attachment cleanup failed"
            );
        }
    }
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Mark one in-app notification as read (must belong to the caller’s `user` id in the JWT).
    async fn mark_notification_read(
        &self,
        ctx: &Context<'_>,
        id: ID,
    ) -> Result<NotificationDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let nid = parse_uuid(&id, "id")?;
        let m = notification_service::mark_read(&db, tenant_id, claims.sub, nid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(NotificationDto::from(m))
    }

    /// Mark every unread notification for this user as read. Returns how many rows were updated.
    async fn mark_all_notifications_read(&self, ctx: &Context<'_>) -> Result<u64> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let n = notification_service::mark_all_read(&db, tenant_id, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(n)
    }

    /// Public bulletin visible to all authenticated users in the tenant (company news or employee post).
    async fn create_announcement(
        &self,
        ctx: &Context<'_>,
        input: CreateAnnouncementInput,
    ) -> Result<AnnouncementDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;

        let title = input.title.trim().to_string();
        if title.is_empty() {
            return Err(KabiPayError::Validation("title must not be empty".into()).into_graphql());
        }

        let hr_only = !input.employee_post
            || input.target_department_id.is_some()
            || input.target_location_id.is_some()
            || input
                .target_role_code
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
            || input.publish_at.is_some()
            || input.expires_at.is_some();
        if hr_only && !claims.can_manage_notifications() {
            return Err(
                KabiPayError::Forbidden(
                    "company posts, scheduling, or audience targeting require notification admin"
                        .into(),
                )
                .into_graphql(),
            );
        }

        let post_source = if input.employee_post {
            "employee_post".to_string()
        } else {
            "company".to_string()
        };

        let target_department_id = input
            .target_department_id
            .as_ref()
            .map(|id| parse_uuid(id, "targetDepartmentId"))
            .transpose()?;
        let target_location_id = input
            .target_location_id
            .as_ref()
            .map(|id| parse_uuid(id, "targetLocationId"))
            .transpose()?;
        let target_audience =
            merged_target_audience(input.target_audience.clone(), input.target_role_code.clone())?;

        let image_file_storage_id =
            maybe_store_image(&db, tenant_id, Some(claims.sub), &input).await?;
        let document_file_storage_id =
            match maybe_store_doc(&db, tenant_id, Some(claims.sub), &input).await {
                Ok(file_id) => file_id,
                Err(error) => {
                    cleanup_attachment_ids(&db, tenant_id, image_file_storage_id).await;
                    return Err(error);
                }
            };

        let created = notification_service::create_announcement(
            &db,
            tenant_id,
            claims.sub,
            notification_service::NewAnnouncement {
                title,
                body: input.body,
                target_audience,
                post_source,
                image_file_storage_id,
                document_file_storage_id,
                target_department_id,
                target_location_id,
                publish_at: input.publish_at,
                expires_at: input.expires_at,
            },
        )
        .await;
        let row = match created {
            Ok(row) => row,
            Err(error) => {
                cleanup_attachment_ids(
                    &db,
                    tenant_id,
                    image_file_storage_id.into_iter().chain(document_file_storage_id),
                )
                .await;
                return Err(error.into_graphql());
            }
        };
        Ok(AnnouncementDto::from(row))
    }

    async fn update_announcement(
        &self,
        ctx: &Context<'_>,
        input: UpdateAnnouncementInput,
    ) -> Result<AnnouncementDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let aid = parse_uuid(&input.id, "id")?;
        let existing = notification_service::get_announcement(&db, tenant_id, aid)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "announcement",
                    id: aid.to_string(),
                }
                .into_graphql()
            })?;
        if !can_edit_announcement(claims, &existing) {
            return Err(KabiPayError::Forbidden("cannot edit this announcement".into()).into_graphql());
        }
        let full = claims.can_manage_notifications();
        if !full
            && (input.clear_target_department
                || input.clear_target_location
                || input.target_department_id.is_some()
                || input.target_location_id.is_some()
                || input.target_role_code.as_ref().map(|s| !s.trim().is_empty()).unwrap_or(false)
                || input.clear_role_audience
                || input.publish_at.is_some()
                || input.clear_publish_at
                || input.expires_at.is_some()
                || input.clear_expires_at)
        {
            return Err(
                KabiPayError::Forbidden("only HR can change audience or schedule".into()).into_graphql(),
            );
        }

        // Resolve every fallible non-storage input before persisting replacement
        // bytes so validation failures cannot orphan newly stored objects.
        let td_patch = if input.clear_target_department {
            Some(None)
        } else if let Some(ref id) = input.target_department_id {
            Some(Some(parse_uuid(id, "targetDepartmentId")?))
        } else {
            None
        };
        let tl_patch = if input.clear_target_location {
            Some(None)
        } else if let Some(ref id) = input.target_location_id {
            Some(Some(parse_uuid(id, "targetLocationId")?))
        } else {
            None
        };
        let (clear_role_aud, aud_patch) = if input.clear_role_audience {
            (true, None)
        } else if input.target_audience.is_some()
            || input
                .target_role_code
                .as_ref()
                .map(|s| !s.trim().is_empty())
                .unwrap_or(false)
        {
            (
                false,
                merged_target_audience(
                    input.target_audience.clone(),
                    input.target_role_code.clone(),
                )?,
            )
        } else {
            (false, None)
        };

        let img_input = CreateAnnouncementInput {
            title: String::new(),
            body: None,
            target_audience: None,
            target_department_id: None,
            target_location_id: None,
            target_role_code: None,
            publish_at: None,
            expires_at: None,
            employee_post: true,
            image_file_name: input.image_file_name.clone(),
            image_mime_type: input.image_mime_type.clone(),
            image_content_base64: input.image_content_base64.clone(),
            document_file_name: input.document_file_name.clone(),
            document_mime_type: input.document_mime_type.clone(),
            document_content_base64: input.document_content_base64.clone(),
        };

        let mut newly_stored_ids = Vec::with_capacity(2);
        let image_patch = if input.clear_image {
            Some(None)
        } else if input
            .image_content_base64
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            let file_id = maybe_store_image(&db, tenant_id, Some(claims.sub), &img_input)
                    .await?
                    .ok_or_else(|| {
                        KabiPayError::Validation("image upload expected".into()).into_graphql()
                    })?;
            newly_stored_ids.push(file_id);
            Some(Some(file_id))
        } else {
            None
        };

        let doc_patch = if input.clear_document {
            Some(None)
        } else if input
            .document_content_base64
            .as_ref()
            .map(|s| !s.trim().is_empty())
            .unwrap_or(false)
        {
            let stored = maybe_store_doc(&db, tenant_id, Some(claims.sub), &img_input).await;
            let file_id = match stored {
                Ok(Some(file_id)) => file_id,
                Ok(None) => {
                    cleanup_attachment_ids(&db, tenant_id, newly_stored_ids).await;
                    return Err(KabiPayError::Validation("document upload expected".into())
                        .into_graphql());
                }
                Err(error) => {
                    cleanup_attachment_ids(&db, tenant_id, newly_stored_ids).await;
                    return Err(error);
                }
            };
            newly_stored_ids.push(file_id);
            Some(Some(file_id))
        } else {
            None
        };

        let patch = notification_service::AnnouncementUpdate {
            title: input.title,
            body: input.body,
            clear_target_audience: clear_role_aud,
            target_audience: aud_patch,
            target_department_id: td_patch,
            target_location_id: tl_patch,
            publish_at: if input.clear_publish_at {
                Some(None)
            } else {
                input.publish_at.map(Some)
            },
            expires_at: if input.clear_expires_at {
                Some(None)
            } else {
                input.expires_at.map(Some)
            },
            image_file_storage_id: image_patch,
            document_file_storage_id: doc_patch,
        };

        let updated = notification_service::update_announcement(&db, tenant_id, aid, patch).await;
        let row = match updated {
            Ok(row) => row,
            Err(error) => {
                cleanup_attachment_ids(&db, tenant_id, newly_stored_ids).await;
                return Err(error.into_graphql());
            }
        };
        Ok(AnnouncementDto::from(row))
    }

    async fn delete_announcement(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let aid = parse_uuid(&id, "id")?;
        let existing = notification_service::get_announcement(&db, tenant_id, aid)
            .await
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| {
                KabiPayError::NotFound {
                    entity: "announcement",
                    id: aid.to_string(),
                }
                .into_graphql()
            })?;
        if !can_edit_announcement(claims, &existing) {
            return Err(KabiPayError::Forbidden("cannot delete this announcement".into()).into_graphql());
        }
        notification_service::delete_announcement(&db, tenant_id, aid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn create_direct_notifications(
        &self,
        ctx: &Context<'_>,
        input: CreateDirectNotificationsInput,
    ) -> Result<u64> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_notifications() {
            return Err(
                KabiPayError::Forbidden("notification:manage or equivalent role required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let uids: Result<Vec<Uuid>> = input
            .user_ids
            .iter()
            .map(|id| parse_uuid(id, "userId"))
            .collect();
        notification_service::create_notifications_for_users(
            &db,
            tenant_id,
            uids?,
            input.kind,
            input.title,
            input.message,
            input.action_url,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    async fn update_notification_admin(
        &self,
        ctx: &Context<'_>,
        input: UpdateNotificationAdminInput,
    ) -> Result<NotificationDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_notifications() {
            return Err(
                KabiPayError::Forbidden("notification:manage or equivalent role required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let nid = parse_uuid(&input.id, "id")?;
        let row = notification_service::update_notification_admin(
            &db,
            tenant_id,
            nid,
            notification_service::NotificationPatch {
                kind: input.kind,
                title: input.title,
                message: input.message,
                action_url: input.action_url,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(NotificationDto::from(row))
    }

    async fn delete_notification_admin(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_notifications() {
            return Err(
                KabiPayError::Forbidden("notification:manage or equivalent role required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let nid = parse_uuid(&id, "id")?;
        notification_service::delete_notification_admin(&db, tenant_id, nid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn update_notification_preferences(
        &self,
        ctx: &Context<'_>,
        input: UpdateNotificationPreferencesInput,
    ) -> Result<NotificationPreferencesGql> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        notification_preference::upsert_notification_prefs(
            &db,
            tenant_id,
            claims.sub,
            input.in_app_enabled,
            input.announcements_enabled,
            input.muted_topics,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let p = notification_preference::load_notification_prefs(&db, tenant_id, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(NotificationPreferencesGql::from_prefs(p))
    }
}
