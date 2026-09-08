//! Write operations for notifications (read state) and public announcements.

use async_graphql::{Context, Object, Result, ID};
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use kabipay_common::{
    client_data_scope::data_scope_from_context,
    context::{ScopeType, PERM_NOTIFICATION_MANAGE, PERM_NOTIFICATION_READ},
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AnnouncementDto, CreateAnnouncementInput, CreateDirectNotificationsInput,
    CelebrationPreferencesGql, NotificationAutomationSettingsGql, NotificationDto,
    NotificationPreferencesGql, SaveNotificationAutomationSettingsInput,
    UpdateAnnouncementInput, UpdateCelebrationPreferencesInput, UpdateNotificationAdminInput,
    UpdateNotificationPreferencesInput,
};
use crate::services::announcement_storage;
use crate::services::automation_settings;
use crate::services::notification_preference;
use crate::services::notification_action;
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

fn require_notification_read(ctx: &Context<'_>) -> Result<()> {
    data_scope_from_context(ctx, PERM_NOTIFICATION_READ).map(|_| ())
}

fn require_notification_manage_all(ctx: &Context<'_>) -> Result<()> {
    let scope = data_scope_from_context(ctx, PERM_NOTIFICATION_MANAGE)?;
    if scope != ScopeType::All {
        return Err(KabiPayError::Forbidden(format!(
            "{PERM_NOTIFICATION_MANAGE} permission requires ALL scope"
        ))
        .into_graphql());
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    use super::MutationRoot;
    use async_graphql::{EmptySubscription, Object, Request, Schema};
    use kabipay_common::context::{
        ClientClaims, CLIENT_JWT_ISSUER, PERM_EMPLOYEE_READ, PERM_NOTIFICATION_MANAGE,
        PERM_NOTIFICATION_READ,
    };
    use kabipay_common::subgraph::TenantId;
    use std::collections::HashMap;
    use uuid::Uuid;

    struct TestQuery;

    #[Object]
    impl TestQuery {
        async fn health(&self) -> bool {
            true
        }
    }

    fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: vec![],
            permissions: vec![permission.into()],
            permission_scopes: scope
                .map(|scope| HashMap::from([(permission.into(), scope.into())]))
                .unwrap_or_default(),
            resource_scopes: HashMap::new(),
        }
    }

    async fn execute(claims: ClientClaims, mutation: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(TestQuery, MutationRoot, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(mutation))
            .await
    }

    fn assert_forbidden_before_db(response: &async_graphql::Response, permission: &str) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(permission) && message.to_ascii_lowercase().contains("permission"),
            "unexpected authorization error: {message}"
        );
        assert!(!message.to_ascii_lowercase().contains("database"));
    }

    #[tokio::test]
    async fn automation_settings_mutation_requires_exact_manage_all_before_db_access() {
        let mutation = r#"mutation {
            saveNotificationAutomationSettings(input: {
                birthdayEnabled: true,
                workAnniversaryEnabled: true,
                companySharingEnabled: true,
                deliveryLocalTime: "09:00:00",
                birthdayTitleTemplate: "Happy birthday, {employee_name}!",
                birthdayMessageTemplate: "Happy birthday, {employee_name}!",
                anniversaryTitleTemplate: "Work anniversary: {employee_name}",
                anniversaryMessageTemplate: "{employee_name}: {service_years} years"
            }) { __typename }
        }"#;

        for denied in [
            claims(PERM_EMPLOYEE_READ, Some("ALL")),
            claims(PERM_NOTIFICATION_READ, Some("ALL")),
            claims(PERM_NOTIFICATION_MANAGE, Some("SELF")),
            claims(PERM_NOTIFICATION_MANAGE, None),
        ] {
            assert_forbidden_before_db(
                &execute(denied, mutation).await,
                PERM_NOTIFICATION_MANAGE,
            );
        }
    }

    #[tokio::test]
    async fn celebration_consent_mutation_requires_exact_read_permission_before_db_access() {
        let mutation = r#"mutation {
            updateMyCelebrationPreferences(input: {
                shareBirthday: true,
                shareWorkAnniversary: false
            }) { __typename }
        }"#;

        for denied in [
            claims(PERM_EMPLOYEE_READ, Some("ALL")),
            claims(PERM_NOTIFICATION_MANAGE, Some("ALL")),
            claims(PERM_NOTIFICATION_READ, None),
        ] {
            assert_forbidden_before_db(&execute(denied, mutation).await, PERM_NOTIFICATION_READ);
        }
    }
}

#[Object]
impl MutationRoot {
    /// Admin / HR: save tenant-wide automated employee-event settings.
    async fn save_notification_automation_settings(
        &self,
        ctx: &Context<'_>,
        input: SaveNotificationAutomationSettingsInput,
    ) -> Result<NotificationAutomationSettingsGql> {
        require_notification_manage_all(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        automation_settings::save_automation_settings(
            &db,
            tenant_id,
            claims.sub,
            input.into(),
        )
        .await
        .map(NotificationAutomationSettingsGql::from)
        .map_err(KabiPayError::into_graphql)
    }

    /// Save company-sharing consent only for the signed-in employee.
    async fn update_my_celebration_preferences(
        &self,
        ctx: &Context<'_>,
        input: UpdateCelebrationPreferencesInput,
    ) -> Result<CelebrationPreferencesGql> {
        require_notification_read(ctx)?;
        let claims = require_client_claims(ctx)?;
        let employee_id = claims.employee_id.ok_or_else(|| {
            KabiPayError::Forbidden("a linked employee profile is required".into()).into_graphql()
        })?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        automation_settings::save_celebration_preferences(
            &db,
            tenant_id,
            employee_id,
            claims.sub,
            input.into(),
        )
        .await
        .map(CelebrationPreferencesGql::from)
        .map_err(KabiPayError::into_graphql)
    }

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
    async fn prepare_announcement_video_upload(&self, ctx: &Context<'_>, file_name: String, mime_type: String, file_size_bytes: i32) -> Result<crate::services::announcement_video::AnnouncementVideoUpload> {
        let tenant = require_tenant_id(ctx)?;
        let owner = require_client_claims(ctx)?.sub;
        let db = crate::services::announcement_video::required_db(ctx, tenant).await?;
        crate::services::announcement_video::prepare(&db, tenant, owner, file_name, mime_type, file_size_bytes).await.map_err(KabiPayError::into_graphql)
    }

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
                video_upload_stage_id: input.video_upload_stage_id,
                video_link: input.video_link,
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
            video_upload_stage_id: None,
            video_link: None,
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
            video_upload_stage_id: input.video_upload_stage_id,
            video_link: input.video_link,
            remove_video: input.remove_video.unwrap_or(false),
            video_owner: claims.sub,
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
                KabiPayError::Forbidden("notification:manage permission required".into())
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
        let action_url = match input.action_url {
            Some(raw) => notification_action::NotificationAction::parse_internal_route(&raw)
                .map_err(KabiPayError::into_graphql)?,
            None => None,
        };
        notification_service::create_notifications_for_users(
            &db,
            tenant_id,
            uids?,
            input.kind,
            input.title,
            input.message,
            action_url,
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
                KabiPayError::Forbidden("notification:manage permission required".into())
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
                action_url: match input.action_url {
                    Some(raw) => Some(
                        notification_action::NotificationAction::parse_internal_route(&raw)
                            .map_err(KabiPayError::into_graphql)?
                            .map(notification_action::NotificationAction::into_string),
                    ),
                    None => None,
                },
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
                KabiPayError::Forbidden("notification:manage permission required".into())
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
