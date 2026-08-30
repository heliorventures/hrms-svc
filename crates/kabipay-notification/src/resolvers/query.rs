//! Root query resolvers for kabipay-notification.

use async_graphql::{Context, Enum, ID, Object, Result};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use chrono::{DateTime, Utc};
use kabipay_common::{
    client_data_scope::data_scope_from_context,
    context::{ScopeType, PERM_NOTIFICATION_MANAGE, PERM_NOTIFICATION_READ},
    subgraph::{
        require_client_claims, require_tenant_id, tenant_db, try_client_employee_dept_and_location,
    },
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0027_communication_audit::announcement;
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

fn announcement_available_to_reader(
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

fn announcement_attachment_not_found(announcement_id: Uuid) -> async_graphql::Error {
    KabiPayError::NotFound {
        entity: "announcementAttachment",
        id: announcement_id.to_string(),
    }
    .into_graphql()
}

async fn resolve_announcement_attachment_with<
    LoadParent,
    ParentFuture,
    LoadFile,
    FileFuture,
    LoadBlob,
    BlobFuture,
>(
    tenant_id: Uuid,
    announcement_id: Uuid,
    kind: AnnouncementAttachmentKind,
    announcements_enabled: bool,
    now: DateTime<Utc>,
    viewer_department: Option<Uuid>,
    viewer_location: Option<Uuid>,
    viewer_roles: &[String],
    load_parent: LoadParent,
    load_file: LoadFile,
    load_blob: LoadBlob,
) -> Result<AnnouncementAttachmentDto>
where
    LoadParent: FnOnce() -> ParentFuture,
    ParentFuture: std::future::Future<Output = KabiPayResult<Option<announcement::Model>>>,
    LoadFile: FnOnce(Uuid) -> FileFuture,
    FileFuture: std::future::Future<Output = KabiPayResult<Option<file_storage::Model>>>,
    LoadBlob: FnOnce(file_storage::Model) -> BlobFuture,
    BlobFuture: std::future::Future<Output = KabiPayResult<Vec<u8>>>,
{
    let row = load_parent()
        .await
        .map_err(KabiPayError::into_graphql)?
        .filter(|row| {
            row.tenant_id == tenant_id
                && announcement_available_to_reader(
                    row,
                    announcements_enabled,
                    now,
                    viewer_department,
                    viewer_location,
                    viewer_roles,
                )
        })
        .ok_or_else(|| announcement_attachment_not_found(announcement_id))?;

    let file_id = attachment_storage_id(
        row.image_file_storage_id,
        row.document_file_storage_id,
        kind,
    )
    .ok_or_else(|| announcement_attachment_not_found(announcement_id))?;
    let stored_file = load_file(file_id)
        .await
        .map_err(KabiPayError::into_graphql)?
        .filter(|stored_file| stored_file.tenant_id == tenant_id)
        .ok_or_else(|| announcement_attachment_not_found(announcement_id))?;
    if stored_file
        .file_size_bytes
        .is_some_and(|size| size > MAX_INLINE_ANNOUNCEMENT_ATTACHMENT_BYTES as i64)
    {
        return Err(KabiPayError::Validation(
            "announcement attachment is too large to return inline".into(),
        )
        .into_graphql());
    }

    let file_name = stored_file
        .original_filename
        .as_ref()
        .filter(|name| !name.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "attachment".into());
    let mime_type = stored_file
        .mime_type
        .as_ref()
        .filter(|mime| !mime.trim().is_empty())
        .cloned()
        .unwrap_or_else(|| "application/octet-stream".into());
    let file_size_bytes = stored_file
        .file_size_bytes
        .and_then(|size| i32::try_from(size).ok());
    let bytes = load_blob(stored_file).await.map_err(|error| match error {
        KabiPayError::NotFound { .. } => announcement_attachment_not_found(announcement_id),
        other => other.into_graphql(),
    })?;
    if bytes.len() > MAX_INLINE_ANNOUNCEMENT_ATTACHMENT_BYTES {
        return Err(KabiPayError::Validation(
            "announcement attachment is too large to return inline".into(),
        )
        .into_graphql());
    }

    Ok(AnnouncementAttachmentDto {
        file_name,
        mime_type,
        file_size_bytes,
        content_base64: STANDARD.encode(bytes),
    })
}

#[cfg(test)]
mod tests {
    use super::{
        announcement_available_to_reader, announcement_is_currently_visible,
        attachment_storage_id, resolve_announcement_attachment_with,
        AnnouncementAttachmentKind, QueryRoot,
    };
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    use chrono::{Duration, Utc};
    use kabipay_common::context::{
        ClientClaims, CLIENT_JWT_ISSUER, PERM_EMPLOYEE_READ, PERM_NOTIFICATION_MANAGE,
        PERM_NOTIFICATION_READ,
    };
    use kabipay_common::subgraph::TenantId;
    use kabipay_db_entities::tenant::{
        d0027_communication_audit::announcement, d0029_file_storage::file_storage,
    };
    use std::collections::HashMap;
    use std::sync::{Arc, Mutex};
    use uuid::Uuid;

    fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
        let permission_scopes = scope
            .map(|scope| HashMap::from([(permission.to_string(), scope.to_string())]))
            .unwrap_or_default();
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
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    async fn execute_query(claims: ClientClaims, query: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(query))
            .await
    }

    fn assert_forbidden_before_db(response: &async_graphql::Response, permission: &str) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(permission) && message.to_ascii_lowercase().contains("permission"),
            "unexpected authorization error: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.to_ascii_lowercase().contains("database"));
    }

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

    #[test]
    fn attachment_parent_must_be_current_and_match_the_reader_audience() {
        let now = Utc::now();
        let tenant_id = Uuid::new_v4();
        let department_id = Uuid::new_v4();
        let location_id = Uuid::new_v4();
        let row = announcement::Model {
            id: Uuid::new_v4(),
            tenant_id,
            created_by: None,
            title: "Team update".into(),
            body: None,
            target_audience: Some("ROLE:MANAGER".into()),
            target_department_id: Some(department_id),
            target_location_id: Some(location_id),
            publish_at: Some(now - Duration::minutes(1)),
            expires_at: Some(now + Duration::minutes(1)),
            image_file_storage_id: Some(Uuid::new_v4()),
            document_file_storage_id: None,
            post_source: "company".into(),
            created_at: now,
            updated_at: now,
        };

        assert!(!announcement_available_to_reader(
            &row,
            false,
            now,
            Some(department_id),
            Some(location_id),
            &["MANAGER".into()],
        ));
        assert!(!announcement_available_to_reader(
            &row,
            true,
            now,
            Some(department_id),
            Some(location_id),
            &["EMPLOYEE".into()],
        ));
        assert!(!announcement_available_to_reader(
            &row,
            true,
            now,
            Some(Uuid::new_v4()),
            Some(location_id),
            &["MANAGER".into()],
        ));
        assert!(announcement_available_to_reader(
            &row,
            true,
            now,
            Some(department_id),
            Some(location_id),
            &["MANAGER".into()],
        ));

        let scheduled = announcement::Model {
            publish_at: Some(now + Duration::minutes(1)),
            ..row
        };
        assert!(!announcement_available_to_reader(
            &scheduled,
            true,
            now,
            Some(department_id),
            Some(location_id),
            &["MANAGER".into()],
        ));
    }

    fn attachment_file(tenant_id: Uuid, file_id: Uuid, now: chrono::DateTime<Utc>) -> file_storage::Model {
        file_storage::Model {
            id: file_id,
            tenant_id,
            provider: "local".into(),
            bucket: None,
            storage_path: "tenant/announcement/file".into(),
            original_filename: Some("notice.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size_bytes: Some(7),
            is_public: false,
            uploaded_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn graphql_error_signature(error: async_graphql::Error) -> (String, async_graphql::Value) {
        let code = error
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("code"))
            .cloned()
            .expect("outward GraphQL error must include a code");
        (error.message, code)
    }

    #[tokio::test]
    async fn hidden_and_missing_attachment_parents_share_one_not_found_and_stop_loading() {
        let now = Utc::now();
        let tenant_id = Uuid::new_v4();
        let department_id = Uuid::new_v4();
        let location_id = Uuid::new_v4();
        let file_id = Uuid::new_v4();
        let base = announcement::Model {
            id: Uuid::new_v4(),
            tenant_id,
            created_by: None,
            title: "Team update".into(),
            body: None,
            target_audience: Some("ROLE:MANAGER".into()),
            target_department_id: Some(department_id),
            target_location_id: Some(location_id),
            publish_at: Some(now - Duration::minutes(1)),
            expires_at: Some(now + Duration::minutes(1)),
            image_file_storage_id: Some(file_id),
            document_file_storage_id: None,
            post_source: "company".into(),
            created_at: now,
            updated_at: now,
        };
        let hidden_cases = [
            ("missing", None, true, vec!["MANAGER".into()]),
            (
                "expired",
                Some(announcement::Model {
                    expires_at: Some(now),
                    ..base.clone()
                }),
                true,
                vec!["MANAGER".into()],
            ),
            (
                "scheduled",
                Some(announcement::Model {
                    publish_at: Some(now + Duration::minutes(1)),
                    ..base.clone()
                }),
                true,
                vec!["MANAGER".into()],
            ),
            (
                "preference-disabled",
                Some(base.clone()),
                false,
                vec!["MANAGER".into()],
            ),
            (
                "wrong-audience",
                Some(base.clone()),
                true,
                vec!["EMPLOYEE".into()],
            ),
            (
                "wrong-tenant",
                Some(announcement::Model {
                    tenant_id: Uuid::new_v4(),
                    ..base.clone()
                }),
                true,
                vec!["MANAGER".into()],
            ),
        ];

        let mut expected_signature = None;
        for (case, parent, announcements_enabled, roles) in hidden_cases {
            let calls = Arc::new(Mutex::new(Vec::new()));
            let parent_calls = Arc::clone(&calls);
            let file_calls = Arc::clone(&calls);
            let blob_calls = Arc::clone(&calls);
            let result = resolve_announcement_attachment_with(
                tenant_id,
                base.id,
                AnnouncementAttachmentKind::Image,
                announcements_enabled,
                now,
                Some(department_id),
                Some(location_id),
                &roles,
                move || async move {
                    parent_calls.lock().expect("call log").push("parent");
                    Ok(parent)
                },
                move |_| async move {
                    file_calls.lock().expect("call log").push("file");
                    Ok(Some(attachment_file(tenant_id, file_id, now)))
                },
                move |_| async move {
                    blob_calls.lock().expect("call log").push("blob");
                    Ok(Vec::new())
                },
            )
            .await;

            let signature = graphql_error_signature(
                result.expect_err("hidden or missing parent must not expose an attachment"),
            );
            assert_eq!(signature.1, async_graphql::Value::from("NOT_FOUND"), "{case}");
            if let Some(expected) = &expected_signature {
                assert_eq!(&signature, expected, "{case}");
            } else {
                expected_signature = Some(signature);
            }
            assert_eq!(
                calls.lock().expect("call log").as_slice(),
                &["parent"],
                "{case} loaded attachment data before parent authorization"
            );
        }
    }

    #[tokio::test]
    async fn authorized_attachment_loads_parent_then_file_then_blob() {
        let now = Utc::now();
        let tenant_id = Uuid::new_v4();
        let department_id = Uuid::new_v4();
        let location_id = Uuid::new_v4();
        let file_id = Uuid::new_v4();
        let announcement_id = Uuid::new_v4();
        let parent = announcement::Model {
            id: announcement_id,
            tenant_id,
            created_by: None,
            title: "Team update".into(),
            body: None,
            target_audience: Some("ROLE:MANAGER".into()),
            target_department_id: Some(department_id),
            target_location_id: Some(location_id),
            publish_at: Some(now - Duration::minutes(1)),
            expires_at: Some(now + Duration::minutes(1)),
            image_file_storage_id: Some(file_id),
            document_file_storage_id: None,
            post_source: "company".into(),
            created_at: now,
            updated_at: now,
        };
        let calls = Arc::new(Mutex::new(Vec::new()));
        let parent_calls = Arc::clone(&calls);
        let file_calls = Arc::clone(&calls);
        let blob_calls = Arc::clone(&calls);

        let attachment = resolve_announcement_attachment_with(
            tenant_id,
            announcement_id,
            AnnouncementAttachmentKind::Image,
            true,
            now,
            Some(department_id),
            Some(location_id),
            &["MANAGER".into()],
            move || async move {
                parent_calls.lock().expect("call log").push("parent");
                Ok(Some(parent))
            },
            move |requested_file_id| async move {
                file_calls.lock().expect("call log").push("file");
                assert_eq!(requested_file_id, file_id);
                Ok(Some(attachment_file(tenant_id, file_id, now)))
            },
            move |stored_file| async move {
                blob_calls.lock().expect("call log").push("blob");
                assert_eq!(stored_file.id, file_id);
                Ok(b"content".to_vec())
            },
        )
        .await
        .expect("authorized parent must load its attachment");

        assert_eq!(calls.lock().expect("call log").as_slice(), &["parent", "file", "blob"]);
        assert_eq!(attachment.file_name, "notice.pdf");
        assert_eq!(attachment.mime_type, "application/pdf");
        assert_eq!(attachment.file_size_bytes, Some(7));
        assert_eq!(attachment.content_base64, STANDARD.encode(b"content"));
    }

    #[tokio::test]
    async fn every_protected_notification_query_requires_its_exact_permission_before_db_access() {
        let announcement_id = Uuid::new_v4();
        let fields = [
            (
                "{ announcements { __typename } }".to_string(),
                PERM_NOTIFICATION_READ,
                false,
            ),
            (
                format!("{{ announcementAttachment(announcementId: \"{announcement_id}\", kind: IMAGE) {{ __typename }} }}"),
                PERM_NOTIFICATION_READ,
                false,
            ),
            (
                "{ adminAnnouncements { __typename } }".to_string(),
                PERM_NOTIFICATION_MANAGE,
                true,
            ),
            (
                "{ notifications { __typename } }".to_string(),
                PERM_NOTIFICATION_READ,
                false,
            ),
            (
                "{ adminNotifications { __typename } }".to_string(),
                PERM_NOTIFICATION_MANAGE,
                true,
            ),
            (
                "{ unreadNotificationCount }".to_string(),
                PERM_NOTIFICATION_READ,
                false,
            ),
            (
                "{ myNotificationPreferences { __typename } }".to_string(),
                PERM_NOTIFICATION_READ,
                false,
            ),
        ];

        for (query, required_permission, requires_all) in fields {
            let sibling_permission = if required_permission == PERM_NOTIFICATION_READ {
                PERM_NOTIFICATION_MANAGE
            } else {
                PERM_NOTIFICATION_READ
            };
            for denied_claims in [
                claims(PERM_EMPLOYEE_READ, Some("ALL")),
                claims(sibling_permission, Some("ALL")),
            ] {
                let response = execute_query(denied_claims, &query).await;
                assert_forbidden_before_db(&response, required_permission);
            }

            for scope in [None, Some("INVALID")] {
                let response = execute_query(claims(required_permission, scope), &query).await;
                assert_forbidden_before_db(&response, required_permission);
            }

            if requires_all {
                for scope in ["SELF", "TEAM", "DEPARTMENT"] {
                    let response =
                        execute_query(claims(required_permission, Some(scope)), &query).await;
                    assert_forbidden_before_db(&response, required_permission);
                }
            }
        }
    }

    #[tokio::test]
    async fn notification_read_and_admin_management_are_not_substitutable() {
        let read_response = execute_query(
            claims(PERM_NOTIFICATION_MANAGE, Some("ALL")),
            "{ notifications { __typename } }",
        )
        .await;
        assert_forbidden_before_db(&read_response, PERM_NOTIFICATION_READ);

        let manage_response = execute_query(
            claims(PERM_NOTIFICATION_READ, Some("ALL")),
            "{ adminNotifications { __typename } }",
        )
        .await;
        assert_forbidden_before_db(&manage_response, PERM_NOTIFICATION_MANAGE);
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
        require_notification_read(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let (viewer_dept, viewer_loc) =
            match try_client_employee_dept_and_location(&db, tenant_id, claims).await {
                Ok(Some((d, l))) => (d, l),
                Ok(None) => (None, None),
                Err(e) => return Err(e.into_graphql()),
            };
        let prefs = crate::services::notification_preference::load_notification_prefs(
            &db,
            tenant_id,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        if !prefs.announcements_enabled {
            return Ok(vec![]);
        }
        let rows = notification_service::list_announcements_visible(
            &db,
            tenant_id,
            limit,
            false,
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
        require_notification_read(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let announcement_id = Uuid::parse_str(announcement_id.as_str()).map_err(|error| {
            KabiPayError::Validation(format!("announcementId: {error}")).into_graphql()
        })?;
        let db = tenant_db(ctx, tenant_id).await?;
        let preferences = crate::services::notification_preference::load_notification_prefs(
            &db,
            tenant_id,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let (viewer_department, viewer_location) =
            match try_client_employee_dept_and_location(&db, tenant_id, claims).await {
                Ok(Some((department, location))) => (department, location),
                Ok(None) => (None, None),
                Err(error) => return Err(error.into_graphql()),
            };

        let db_ref = &db;
        resolve_announcement_attachment_with(
            tenant_id,
            announcement_id,
            kind,
            preferences.announcements_enabled,
            Utc::now(),
            viewer_department,
            viewer_location,
            &claims.roles,
            || notification_service::get_announcement(db_ref, tenant_id, announcement_id),
            |file_id| async move {
                file_storage::Entity::find_by_id(file_id)
                    .filter(file_storage::Column::TenantId.eq(tenant_id))
                    .one(db_ref)
                    .await
                    .map_err(KabiPayError::from)
            },
            |stored_file| async move {
                crate::services::announcement_storage::read_blob(&stored_file).await
            },
        )
        .await
    }

    /// Admin / HR: all recent announcements including scheduled or expired (for management UI).
    async fn admin_announcements(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AnnouncementDto>> {
        require_notification_manage_all(ctx)?;
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
        require_notification_read(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
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
        require_notification_manage_all(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = notification_service::list_notifications(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(NotificationDto::from).collect())
    }

    async fn unread_notification_count(&self, ctx: &Context<'_>) -> Result<u64> {
        require_notification_read(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        notification_service::count_unread_for_user(&db, tenant_id, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    /// Current user’s in-app visibility preferences (announcement bulletin + per-topic mutes).
    async fn my_notification_preferences(&self, ctx: &Context<'_>) -> Result<NotificationPreferencesGql> {
        require_notification_read(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
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
