//! Per-user visibility for in-app notification rows (topic mutes + master toggle).

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use kabipay_db_entities::tenant::d0046_user_notification_preference::user_notification_preference;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, Set,
};
use serde_json::json;
use std::collections::HashSet;
use uuid::Uuid;

/// Canonical topic keys stored in `muted_topics` JSON and accepted by GraphQL.
pub const ALLOWED_MUTED_TOPICS: &[&str] = &[
    "leave", "expense", "travel", "tax", "hr_direct", "celebration", "other",
];

#[derive(Clone, Debug)]
pub struct NotificationPrefs {
    pub in_app_enabled: bool,
    pub announcements_enabled: bool,
    pub muted_topics: HashSet<String>,
}

impl Default for NotificationPrefs {
    fn default() -> Self {
        Self {
            in_app_enabled: true,
            announcements_enabled: true,
            muted_topics: HashSet::new(),
        }
    }
}

impl NotificationPrefs {
    fn from_model(m: &user_notification_preference::Model) -> Self {
        Self {
            in_app_enabled: m.in_app_enabled,
            announcements_enabled: m.announcements_enabled,
            muted_topics: muted_set_from_json(&m.muted_topics),
        }
    }
}

fn muted_set_from_json(j: &sea_orm::prelude::Json) -> HashSet<String> {
    j.as_array()
        .map(|arr| {
            arr.iter()
                .filter_map(|v| v.as_str().map(|s| s.trim().to_lowercase()))
                .filter(|s| !s.is_empty())
                .collect()
        })
        .unwrap_or_default()
}

/// Maps `notification.type` to a stable topic key for muting.
pub fn notification_topic_key(ntype: Option<&str>) -> String {
    let Some(s) = ntype.map(str::trim) else {
        return "other".into();
    };
    if s.is_empty() {
        return "other".into();
    }
    let u = s.to_ascii_uppercase();
    if u == "LEAVE" || u.starts_with("LEAVE_") {
        return "leave".into();
    }
    if u == "EXPENSE" || u.starts_with("EXPENSE") {
        return "expense".into();
    }
    if u == "TRAVEL" || u.starts_with("TRAVEL") {
        return "travel".into();
    }
    if u == "TAX" || u.starts_with("TAX") {
        return "tax".into();
    }
    if matches!(
        u.as_str(),
        "EMPLOYEE_BIRTHDAY" | "EMPLOYEE_WORK_ANNIVERSARY"
    ) {
        return "celebration".into();
    }
    if u.contains("HR") || u.contains("BROADCAST") {
        return "hr_direct".into();
    }
    "other".into()
}

pub fn row_visible(m: &notification::Model, prefs: &NotificationPrefs) -> bool {
    if !prefs.in_app_enabled {
        return false;
    }
    let key = notification_topic_key(m.r#type.as_deref());
    !prefs.muted_topics.contains(&key)
}

pub async fn load_notification_prefs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
) -> KabiPayResult<NotificationPrefs> {
    let row = user_notification_preference::Entity::find()
        .filter(user_notification_preference::Column::TenantId.eq(tenant_id))
        .filter(user_notification_preference::Column::UserId.eq(user_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(match row {
        Some(m) => NotificationPrefs::from_model(&m),
        None => NotificationPrefs::default(),
    })
}

fn normalize_muted_topics(topics: Vec<String>) -> Vec<String> {
    let allowed: HashSet<&str> = ALLOWED_MUTED_TOPICS.iter().copied().collect();
    topics
        .into_iter()
        .map(|s| s.trim().to_lowercase())
        .filter(|s| !s.is_empty() && allowed.contains(s.as_str()))
        .collect()
}

pub async fn upsert_notification_prefs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    user_id: Uuid,
    in_app_enabled: bool,
    announcements_enabled: bool,
    muted_topics: Vec<String>,
) -> KabiPayResult<user_notification_preference::Model> {
    let normalized = normalize_muted_topics(muted_topics);
    let json_val = json!(normalized);
    let now = Utc::now();

    let existing = user_notification_preference::Entity::find()
        .filter(user_notification_preference::Column::TenantId.eq(tenant_id))
        .filter(user_notification_preference::Column::UserId.eq(user_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    if let Some(row) = existing {
        let mut am: user_notification_preference::ActiveModel = row.into();
        am.in_app_enabled = Set(in_app_enabled);
        am.announcements_enabled = Set(announcements_enabled);
        am.muted_topics = Set(json_val);
        am.updated_at = Set(now);
        am.update(db).await.map_err(KabiPayError::from)?;
    } else {
        let am = user_notification_preference::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            user_id: Set(user_id),
            in_app_enabled: Set(in_app_enabled),
            announcements_enabled: Set(announcements_enabled),
            muted_topics: Set(json_val),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await.map_err(KabiPayError::from)?;
    }

    user_notification_preference::Entity::find()
        .filter(user_notification_preference::Column::TenantId.eq(tenant_id))
        .filter(user_notification_preference::Column::UserId.eq(user_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| {
            KabiPayError::Internal("user_notification_preference missing after upsert".into())
        })
}

#[cfg(test)]
mod celebration_topic_tests {
    use super::*;

    #[test]
    fn automated_employee_events_use_the_celebration_topic() {
        assert_eq!(
            notification_topic_key(Some("EMPLOYEE_BIRTHDAY")),
            "celebration"
        );
        assert_eq!(
            notification_topic_key(Some("EMPLOYEE_WORK_ANNIVERSARY")),
            "celebration"
        );
        assert!(ALLOWED_MUTED_TOPICS.contains(&"celebration"));
    }
}
