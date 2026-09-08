//! Tenant notification automation settings and employee-owned celebration consent.

use chrono::{NaiveTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0027_communication_audit::audit_log,
    d0074_automated_employee_notifications::{
        employee_celebration_preference, notification_automation_setting,
    },
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

use super::automated_events::{validate_template, CelebrationEventKind};

const DEFAULT_BIRTHDAY_TITLE: &str = "Happy birthday, {employee_name}!";
const DEFAULT_BIRTHDAY_MESSAGE: &str = "Wishing {employee_name} a wonderful birthday.";
const DEFAULT_ANNIVERSARY_TITLE: &str = "Work anniversary: {employee_name}";
const DEFAULT_ANNIVERSARY_MESSAGE: &str =
    "Celebrating {employee_name}'s {service_years}-year work anniversary.";

/// Tenant-wide automatic notification configuration.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct AutomationSettings {
    pub birthday_enabled: bool,
    pub work_anniversary_enabled: bool,
    pub company_sharing_enabled: bool,
    pub delivery_local_time: NaiveTime,
    pub birthday_title_template: String,
    pub birthday_message_template: String,
    pub anniversary_title_template: String,
    pub anniversary_message_template: String,
}

impl Default for AutomationSettings {
    fn default() -> Self {
        Self {
            birthday_enabled: true,
            work_anniversary_enabled: true,
            company_sharing_enabled: true,
            delivery_local_time: NaiveTime::from_hms_opt(9, 0, 0)
                .expect("09:00:00 is a valid fixed default"),
            birthday_title_template: DEFAULT_BIRTHDAY_TITLE.into(),
            birthday_message_template: DEFAULT_BIRTHDAY_MESSAGE.into(),
            anniversary_title_template: DEFAULT_ANNIVERSARY_TITLE.into(),
            anniversary_message_template: DEFAULT_ANNIVERSARY_MESSAGE.into(),
        }
    }
}

impl From<notification_automation_setting::Model> for AutomationSettings {
    fn from(model: notification_automation_setting::Model) -> Self {
        Self {
            birthday_enabled: model.birthday_enabled,
            work_anniversary_enabled: model.work_anniversary_enabled,
            company_sharing_enabled: model.company_sharing_enabled,
            delivery_local_time: model.delivery_local_time,
            birthday_title_template: model.birthday_title_template,
            birthday_message_template: model.birthday_message_template,
            anniversary_title_template: model.anniversary_title_template,
            anniversary_message_template: model.anniversary_message_template,
        }
    }
}

/// Validated values accepted by the Admin/HR settings mutation.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct SaveAutomationSettings {
    pub birthday_enabled: bool,
    pub work_anniversary_enabled: bool,
    pub company_sharing_enabled: bool,
    pub delivery_local_time: NaiveTime,
    pub birthday_title_template: String,
    pub birthday_message_template: String,
    pub anniversary_title_template: String,
    pub anniversary_message_template: String,
}

impl From<AutomationSettings> for SaveAutomationSettings {
    fn from(settings: AutomationSettings) -> Self {
        Self {
            birthday_enabled: settings.birthday_enabled,
            work_anniversary_enabled: settings.work_anniversary_enabled,
            company_sharing_enabled: settings.company_sharing_enabled,
            delivery_local_time: settings.delivery_local_time,
            birthday_title_template: settings.birthday_title_template,
            birthday_message_template: settings.birthday_message_template,
            anniversary_title_template: settings.anniversary_title_template,
            anniversary_message_template: settings.anniversary_message_template,
        }
    }
}

/// Employee-owned company-sharing consent. Missing rows are private by default.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CelebrationPreferences {
    pub share_birthday: bool,
    pub share_work_anniversary: bool,
}

impl From<employee_celebration_preference::Model> for CelebrationPreferences {
    fn from(model: employee_celebration_preference::Model) -> Self {
        Self {
            share_birthday: model.share_birthday,
            share_work_anniversary: model.share_work_anniversary,
        }
    }
}

fn validate_title(value: &str, kind: CelebrationEventKind) -> KabiPayResult<String> {
    validate_template(value, kind)?;
    let value = value.trim();
    if value.chars().count() > 500 {
        return Err(KabiPayError::Validation(
            "notification title template must contain 1 to 500 characters".into(),
        ));
    }
    Ok(value.into())
}

fn validate_message(value: &str, kind: CelebrationEventKind) -> KabiPayResult<String> {
    validate_template(value, kind)?;
    Ok(value.trim().into())
}

/// Parses settings into a normalized form before any database access.
pub fn validate_settings(input: &SaveAutomationSettings) -> KabiPayResult<SaveAutomationSettings> {
    Ok(SaveAutomationSettings {
        birthday_enabled: input.birthday_enabled,
        work_anniversary_enabled: input.work_anniversary_enabled,
        company_sharing_enabled: input.company_sharing_enabled,
        delivery_local_time: input.delivery_local_time,
        birthday_title_template: validate_title(
            &input.birthday_title_template,
            CelebrationEventKind::Birthday,
        )?,
        birthday_message_template: validate_message(
            &input.birthday_message_template,
            CelebrationEventKind::Birthday,
        )?,
        anniversary_title_template: validate_title(
            &input.anniversary_title_template,
            CelebrationEventKind::WorkAnniversary,
        )?,
        anniversary_message_template: validate_message(
            &input.anniversary_message_template,
            CelebrationEventKind::WorkAnniversary,
        )?,
    })
}

/// Loads tenant settings or returns the documented defaults without creating a row.
pub async fn load_automation_settings(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<AutomationSettings> {
    let row = notification_automation_setting::Entity::find()
        .filter(notification_automation_setting::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(row.map(AutomationSettings::from).unwrap_or_default())
}

/// Saves normalized tenant settings and a metadata-only audit entry atomically.
pub async fn save_automation_settings(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    actor_user_id: Uuid,
    input: SaveAutomationSettings,
) -> KabiPayResult<AutomationSettings> {
    let input = validate_settings(&input)?;
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let now = Utc::now();
    let existing = notification_automation_setting::Entity::find()
        .filter(notification_automation_setting::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?;
    let is_new = existing.is_none();
    let mut model = existing
        .map(IntoActiveModel::into_active_model)
        .unwrap_or_else(|| notification_automation_setting::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            created_at: Set(now),
            ..Default::default()
        });
    model.birthday_enabled = Set(input.birthday_enabled);
    model.work_anniversary_enabled = Set(input.work_anniversary_enabled);
    model.company_sharing_enabled = Set(input.company_sharing_enabled);
    model.delivery_local_time = Set(input.delivery_local_time);
    model.birthday_title_template = Set(input.birthday_title_template);
    model.birthday_message_template = Set(input.birthday_message_template);
    model.anniversary_title_template = Set(input.anniversary_title_template);
    model.anniversary_message_template = Set(input.anniversary_message_template);
    model.updated_by = Set(Some(actor_user_id));
    model.updated_at = Set(now);
    let saved = if is_new {
        model.insert(&txn).await
    } else {
        model.update(&txn).await
    }
    .map_err(KabiPayError::from)?;

    audit_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(Some(actor_user_id)),
        entity_type: Set("notification_automation_setting".into()),
        entity_id: Set(Some(saved.id)),
        action: Set("NOTIFICATION_AUTOMATION_UPDATED".into()),
        before_state: Set(None),
        after_state: Set(Some(serde_json::json!({
            "birthdayEnabled": saved.birthday_enabled,
            "workAnniversaryEnabled": saved.work_anniversary_enabled,
            "companySharingEnabled": saved.company_sharing_enabled,
            "deliveryLocalTime": saved.delivery_local_time.to_string(),
        }))),
        ip_address: Set(None),
        user_agent: Set(None),
        created_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(saved.into())
}

/// Loads employee consent or returns private defaults without creating a row.
pub async fn load_celebration_preferences(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<CelebrationPreferences> {
    let row = employee_celebration_preference::Entity::find()
        .filter(employee_celebration_preference::Column::TenantId.eq(tenant_id))
        .filter(employee_celebration_preference::Column::EmployeeId.eq(employee_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(row.map(CelebrationPreferences::from).unwrap_or_default())
}

/// Saves self-owned celebration consent and a metadata-only audit entry atomically.
pub async fn save_celebration_preferences(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    actor_user_id: Uuid,
    input: CelebrationPreferences,
) -> KabiPayResult<CelebrationPreferences> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let now = Utc::now();
    let existing = employee_celebration_preference::Entity::find()
        .filter(employee_celebration_preference::Column::TenantId.eq(tenant_id))
        .filter(employee_celebration_preference::Column::EmployeeId.eq(employee_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?;
    let is_new = existing.is_none();
    let mut model = existing
        .map(IntoActiveModel::into_active_model)
        .unwrap_or_else(|| employee_celebration_preference::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            created_at: Set(now),
            ..Default::default()
        });
    model.share_birthday = Set(input.share_birthday);
    model.share_work_anniversary = Set(input.share_work_anniversary);
    model.updated_at = Set(now);
    let saved = if is_new {
        model.insert(&txn).await
    } else {
        model.update(&txn).await
    }
    .map_err(KabiPayError::from)?;

    audit_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(Some(actor_user_id)),
        entity_type: Set("employee_celebration_preference".into()),
        entity_id: Set(Some(saved.id)),
        action: Set("CELEBRATION_CONSENT_UPDATED".into()),
        before_state: Set(None),
        after_state: Set(Some(serde_json::json!({
            "shareBirthday": saved.share_birthday,
            "shareWorkAnniversary": saved.share_work_anniversary,
        }))),
        ip_address: Set(None),
        user_agent: Set(None),
        created_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(saved.into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::NaiveTime;

    #[test]
    fn missing_settings_use_enabled_but_privacy_safe_defaults() {
        let settings = AutomationSettings::default();

        assert!(settings.birthday_enabled);
        assert!(settings.work_anniversary_enabled);
        assert!(settings.company_sharing_enabled);
        assert_eq!(
            settings.delivery_local_time,
            NaiveTime::from_hms_opt(9, 0, 0).expect("valid default time")
        );
    }

    #[test]
    fn missing_employee_preferences_do_not_share_events_company_wide() {
        let preferences = CelebrationPreferences::default();

        assert!(!preferences.share_birthday);
        assert!(!preferences.share_work_anniversary);
    }

    #[test]
    fn rejects_unsupported_or_overlong_templates_before_database_access() {
        let mut input = SaveAutomationSettings::from(AutomationSettings::default());
        input.birthday_message_template = "Age {age}".into();
        assert!(validate_settings(&input).is_err());

        input.birthday_message_template = "x".repeat(4_001);
        assert!(validate_settings(&input).is_err());
    }
}
