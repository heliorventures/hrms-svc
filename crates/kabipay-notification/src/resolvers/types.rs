//! GraphQL DTOs for kabipay-notification.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, Utc};
use kabipay_db_entities::tenant::d0027_communication_audit::{announcement, notification};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AnnouncementAttachment")]
pub struct AnnouncementAttachmentDto {
    pub file_name: String,
    pub mime_type: String,
    pub file_size_bytes: Option<i32>,
    pub content_base64: String,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Announcement")]
pub struct AnnouncementDto {
    pub id: ID,
    pub tenant_id: ID,
    pub created_by: Option<ID>,
    pub title: String,
    pub body: Option<String>,
    pub target_audience: Option<String>,
    pub target_department_id: Option<ID>,
    pub target_location_id: Option<ID>,
    pub post_source: String,
    pub has_image_attachment: bool,
    pub has_document_attachment: bool,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<announcement::Model> for AnnouncementDto {
    fn from(m: announcement::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            created_by: m.created_by.map(|u| ID(u.to_string())),
            title: m.title,
            body: m.body,
            target_audience: m.target_audience,
            target_department_id: m.target_department_id.map(|u| ID(u.to_string())),
            target_location_id: m.target_location_id.map(|u| ID(u.to_string())),
            post_source: m.post_source,
            has_image_attachment: m.image_file_storage_id.is_some(),
            has_document_attachment: m.document_file_storage_id.is_some(),
            publish_at: m.publish_at,
            expires_at: m.expires_at,
            created_at: m.created_at,
        }
    }
}

#[cfg(test)]
mod announcement_tests {
    use super::*;
    use uuid::Uuid;

    #[test]
    fn announcement_summary_reports_attachment_presence_without_loading_bytes() {
        let image_id = Uuid::new_v4();
        let model = announcement::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            created_by: Some(Uuid::new_v4()),
            title: "Maintenance".into(),
            body: None,
            target_audience: None,
            target_department_id: None,
            target_location_id: None,
            publish_at: Some(Utc::now()),
            expires_at: None,
            image_file_storage_id: Some(image_id),
            document_file_storage_id: None,
            post_source: "company".into(),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        };

        let dto = AnnouncementDto::from(model);

        assert!(dto.has_image_attachment);
        assert!(!dto.has_document_attachment);
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Notification")]
pub struct NotificationDto {
    pub id: ID,
    pub tenant_id: ID,
    pub user_id: ID,
    #[graphql(name = "kind")]
    pub kind: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub action_url: Option<String>,
    pub is_read: bool,
    pub read_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<notification::Model> for NotificationDto {
    fn from(m: notification::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            user_id: ID(m.user_id.to_string()),
            kind: m.r#type,
            title: m.title,
            message: m.message,
            action_url: m.action_url,
            is_read: m.is_read,
            read_at: m.read_at,
            created_at: m.created_at,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct CreateAnnouncementInput {
    pub title: String,
    pub body: Option<String>,
    pub target_audience: Option<String>,
    /// Broadcast to one department (`employee.department_id` must match). HR / comms only unless left empty.
    pub target_department_id: Option<ID>,
    pub target_location_id: Option<ID>,
    /// When set with `employee_post=false`, stored as `target_audience` `ROLE:<code>` (e.g. `HR_ADMIN`).
    pub target_role_code: Option<String>,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    /// When true (default), marks the row as an employee bulletin (`post_source=employee_post`).
    #[graphql(default = true)]
    pub employee_post: bool,
    pub image_file_name: Option<String>,
    pub image_mime_type: Option<String>,
    /// Standard base64 (not data URL). Max ~6MB decoded.
    pub image_content_base64: Option<String>,
    pub document_file_name: Option<String>,
    pub document_mime_type: Option<String>,
    pub document_content_base64: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateAnnouncementInput {
    pub id: ID,
    pub title: Option<String>,
    pub body: Option<String>,
    pub target_audience: Option<String>,
    pub target_department_id: Option<ID>,
    pub target_location_id: Option<ID>,
    /// Set true to clear department targeting.
    #[graphql(default = false)]
    pub clear_target_department: bool,
    /// Set true to clear location targeting.
    #[graphql(default = false)]
    pub clear_target_location: bool,
    pub target_role_code: Option<String>,
    /// Clears role-based `ROLE:*` targeting when true.
    #[graphql(default = false)]
    pub clear_role_audience: bool,
    pub publish_at: Option<DateTime<Utc>>,
    pub expires_at: Option<DateTime<Utc>>,
    pub clear_publish_at: bool,
    pub clear_expires_at: bool,
    pub image_file_name: Option<String>,
    pub image_mime_type: Option<String>,
    pub image_content_base64: Option<String>,
    pub document_file_name: Option<String>,
    pub document_mime_type: Option<String>,
    pub document_content_base64: Option<String>,
    pub clear_image: bool,
    pub clear_document: bool,
}

#[derive(InputObject, Clone, Debug)]
pub struct CreateDirectNotificationsInput {
    pub user_ids: Vec<ID>,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub action_url: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateNotificationAdminInput {
    pub id: ID,
    pub kind: Option<String>,
    pub title: Option<String>,
    pub message: Option<String>,
    pub action_url: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "NotificationPreferences")]
pub struct NotificationPreferencesGql {
    #[graphql(name = "inAppEnabled")]
    pub in_app_enabled: bool,
    #[graphql(name = "announcementsEnabled")]
    pub announcements_enabled: bool,
    #[graphql(name = "mutedTopics")]
    pub muted_topics: Vec<String>,
}

impl NotificationPreferencesGql {
    pub fn from_prefs(p: crate::services::notification_preference::NotificationPrefs) -> Self {
        let mut topics: Vec<_> = p.muted_topics.into_iter().collect();
        topics.sort();
        Self {
            in_app_enabled: p.in_app_enabled,
            announcements_enabled: p.announcements_enabled,
            muted_topics: topics,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateNotificationPreferencesInput {
    #[graphql(name = "inAppEnabled")]
    pub in_app_enabled: bool,
    #[graphql(name = "announcementsEnabled")]
    pub announcements_enabled: bool,
    #[graphql(name = "mutedTopics")]
    pub muted_topics: Vec<String>,
}
