//! Tenant-local birthday and work-anniversary notification generation.

use chrono::{DateTime, Datelike, NaiveDate, NaiveTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseConnection, DbBackend, Set, Statement,
    TransactionTrait,
};
use std::collections::BTreeSet;
use uuid::Uuid;

use super::automation_settings::{load_automation_settings, AutomationSettings};

/// Supported employee events. The enum prevents unvalidated notification type strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CelebrationEventKind {
    Birthday,
    WorkAnniversary,
}

impl CelebrationEventKind {
    /// Stable value written to `notification.type` and the occurrence ledger.
    #[must_use]
    pub const fn notification_type(self) -> &'static str {
        match self {
            Self::Birthday => "EMPLOYEE_BIRTHDAY",
            Self::WorkAnniversary => "EMPLOYEE_WORK_ANNIVERSARY",
        }
    }
}

/// Validated notification content ready for persistence.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct CelebrationMessage {
    pub notification_type: &'static str,
    pub title: String,
    pub message: String,
}

/// PII-free counters returned to the background worker for operational telemetry.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub struct CelebrationSweepResult {
    pub eligible_events: u64,
    pub notifications_created: u64,
    pub duplicates_skipped: u64,
    pub before_delivery_time: bool,
}

#[derive(Debug)]
struct EligibleEmployee {
    employee_id: Uuid,
    employee_user_id: Uuid,
    manager_user_id: Option<Uuid>,
    employee_code: String,
    first_name: String,
    last_name: String,
    date_of_birth: Option<NaiveDate>,
    date_of_joining: NaiveDate,
    share_birthday: bool,
    share_work_anniversary: bool,
}

impl EligibleEmployee {
    fn display_name(&self) -> String {
        let full_name = format!("{} {}", self.first_name.trim(), self.last_name.trim())
            .trim()
            .to_owned();
        if full_name.is_empty() {
            self.employee_code.trim().to_owned()
        } else {
            full_name
        }
    }
}

/// Returns whether the tenant-local clock has reached today's configured delivery time.
#[must_use]
pub fn delivery_time_reached(
    current_local_time: NaiveTime,
    delivery_local_time: NaiveTime,
) -> bool {
    current_local_time >= delivery_local_time
}

/// Resolves the privacy-safe audience for an employee celebration.
///
/// Company-wide delivery requires both the tenant setting and employee consent. Otherwise,
/// the event remains private to the employee and their distinct reporting manager.
#[must_use]
pub fn select_recipient_user_ids(
    employee_user_id: Uuid,
    manager_user_id: Option<Uuid>,
    company_sharing_enabled: bool,
    employee_consented: bool,
    company_user_ids: &[Uuid],
) -> Vec<Uuid> {
    if company_sharing_enabled && employee_consented {
        return company_user_ids
            .iter()
            .copied()
            .collect::<BTreeSet<_>>()
            .into_iter()
            .collect();
    }

    let mut recipients = vec![employee_user_id];
    if let Some(manager_user_id) = manager_user_id.filter(|id| *id != employee_user_id) {
        recipients.push(manager_user_id);
    }
    recipients
}

fn validation(message: impl Into<String>) -> KabiPayError {
    KabiPayError::Validation(message.into())
}

fn observed_date_in_year(source_date: NaiveDate, year: i32) -> Option<NaiveDate> {
    NaiveDate::from_ymd_opt(year, source_date.month(), source_date.day()).or_else(|| {
        (source_date.month() == 2 && source_date.day() == 29)
            .then(|| NaiveDate::from_ymd_opt(year, 2, 28))
            .flatten()
    })
}

/// Returns completed service years as of the supplied tenant-local business date.
#[must_use]
pub fn completed_service_years(
    joined: NaiveDate,
    business_date: NaiveDate,
) -> Option<u32> {
    if business_date < joined {
        return None;
    }
    let anniversary = observed_date_in_year(joined, business_date.year())?;
    let mut years = business_date.year().checked_sub(joined.year())?;
    if business_date < anniversary {
        years = years.checked_sub(1)?;
    }
    u32::try_from(years).ok().filter(|years| *years >= 1)
}

/// Checks whether an employee event is due on the tenant-local business date.
#[must_use]
pub fn is_event_due(
    kind: CelebrationEventKind,
    source_date: NaiveDate,
    business_date: NaiveDate,
) -> bool {
    let Some(observed_date) = observed_date_in_year(source_date, business_date.year()) else {
        return false;
    };
    if observed_date != business_date {
        return false;
    }
    match kind {
        CelebrationEventKind::Birthday => true,
        CelebrationEventKind::WorkAnniversary => {
            completed_service_years(source_date, business_date).is_some()
        }
    }
}

fn token_allowed(token: &str, kind: CelebrationEventKind) -> bool {
    token == "employee_name"
        || (token == "service_years" && kind == CelebrationEventKind::WorkAnniversary)
}

/// Validates the controlled template language without exposing age or birth year tokens.
pub fn validate_template(template: &str, kind: CelebrationEventKind) -> KabiPayResult<()> {
    let trimmed = template.trim();
    if trimmed.is_empty() || trimmed.chars().count() > 4_000 {
        return Err(validation("notification template must contain 1 to 4000 characters"));
    }

    let mut remaining = trimmed;
    while let Some(open) = remaining.find('{') {
        if remaining[..open].contains('}') {
            return Err(validation("notification template contains an unmatched closing brace"));
        }
        let after_open = &remaining[open + 1..];
        let close = after_open
            .find('}')
            .ok_or_else(|| validation("notification template contains an unmatched opening brace"))?;
        let token = &after_open[..close];
        if token.is_empty() || token.contains('{') || !token_allowed(token, kind) {
            return Err(validation(format!(
                "unsupported notification template token {{{token}}}"
            )));
        }
        remaining = &after_open[close + 1..];
    }
    if remaining.contains('}') {
        return Err(validation("notification template contains an unmatched closing brace"));
    }
    Ok(())
}

fn render_template(
    template: &str,
    employee_name: &str,
    service_years: Option<u32>,
) -> String {
    let rendered = template.replace("{employee_name}", employee_name);
    match service_years {
        Some(years) => rendered.replace("{service_years}", &years.to_string()),
        None => rendered,
    }
}

/// Validates and renders safe notification content.
pub fn render_event_message(
    kind: CelebrationEventKind,
    employee_name: &str,
    service_years: Option<u32>,
    title_template: &str,
    message_template: &str,
) -> KabiPayResult<CelebrationMessage> {
    validate_template(title_template, kind)?;
    validate_template(message_template, kind)?;
    let employee_name = employee_name.trim();
    if employee_name.is_empty() || employee_name.chars().count() > 255 {
        return Err(validation("employee display name must contain 1 to 255 characters"));
    }
    if kind == CelebrationEventKind::WorkAnniversary && service_years.is_none() {
        return Err(validation("completed service years are required for a work anniversary"));
    }

    let title = render_template(title_template, employee_name, service_years);
    let message = render_template(message_template, employee_name, service_years);
    if title.trim().is_empty() || title.chars().count() > 500 {
        return Err(validation("notification title must contain 1 to 500 characters"));
    }
    if message.trim().is_empty() || message.chars().count() > 4_000 {
        return Err(validation("notification message must contain 1 to 4000 characters"));
    }

    Ok(CelebrationMessage {
        notification_type: kind.notification_type(),
        title,
        message,
    })
}

async fn load_eligible_employees(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<EligibleEmployee>> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT e.id AS employee_id,
                      e.user_id AS employee_user_id,
                      manager_user.id AS manager_user_id,
                      e.employee_code,
                      e.first_name,
                      e.last_name,
                      e.date_of_birth,
                      e.date_of_joining,
                      COALESCE(preference.share_birthday, FALSE) AS share_birthday,
                      COALESCE(preference.share_work_anniversary, FALSE) AS share_work_anniversary
                 FROM employee e
                 JOIN "user" employee_user
                   ON employee_user.id = e.user_id
                  AND employee_user.tenant_id = e.tenant_id
                  AND employee_user.is_active = TRUE
                  AND employee_user.is_deleted = FALSE
            LEFT JOIN employee manager
                   ON manager.id = e.reporting_manager_id
                  AND manager.tenant_id = e.tenant_id
                  AND UPPER(TRIM(manager.status)) IN ('ACTIVE','PROBATION','ON_LEAVE')
                  AND manager.is_deleted = FALSE
            LEFT JOIN "user" manager_user
                   ON manager_user.id = manager.user_id
                  AND manager_user.tenant_id = e.tenant_id
                  AND manager_user.is_active = TRUE
                  AND manager_user.is_deleted = FALSE
            LEFT JOIN employee_celebration_preference preference
                   ON preference.tenant_id = e.tenant_id
                  AND preference.employee_id = e.id
                WHERE e.tenant_id = $1
                  AND UPPER(TRIM(e.status)) IN ('ACTIVE','PROBATION','ON_LEAVE')
                  AND e.is_deleted = FALSE
             ORDER BY e.id"#,
            vec![tenant_id.into()],
        ))
        .await
        .map_err(KabiPayError::from)?;

    rows.into_iter()
        .map(|row| {
            Ok(EligibleEmployee {
                employee_id: row.try_get("", "employee_id")?,
                employee_user_id: row.try_get("", "employee_user_id")?,
                manager_user_id: row.try_get("", "manager_user_id")?,
                employee_code: row.try_get("", "employee_code")?,
                first_name: row.try_get("", "first_name")?,
                last_name: row.try_get("", "last_name")?,
                date_of_birth: row.try_get("", "date_of_birth")?,
                date_of_joining: row.try_get("", "date_of_joining")?,
                share_birthday: row.try_get("", "share_birthday")?,
                share_work_anniversary: row.try_get("", "share_work_anniversary")?,
            })
        })
        .collect()
}

async fn load_company_notification_users(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT DISTINCT tenant_user.id
                 FROM "user" tenant_user
                 JOIN user_role assigned_role ON assigned_role.user_id = tenant_user.id
                 JOIN role tenant_role
                   ON tenant_role.id = assigned_role.role_id
                  AND tenant_role.tenant_id = tenant_user.tenant_id
                  AND tenant_role.is_deleted = FALSE
                 JOIN role_permission granted_permission
                   ON granted_permission.role_id = tenant_role.id
                 JOIN permission permission_catalog
                   ON permission_catalog.id = granted_permission.permission_id
                WHERE tenant_user.tenant_id = $1
                  AND tenant_user.is_active = TRUE
                  AND tenant_user.is_deleted = FALSE
                  AND permission_catalog.resource = 'notification'
                  AND permission_catalog.action = 'read'
             ORDER BY tenant_user.id"#,
            vec![tenant_id.into()],
        ))
        .await
        .map_err(KabiPayError::from)?;

    rows.into_iter()
        .map(|row| row.try_get("", "id").map_err(KabiPayError::from))
        .collect()
}

async fn create_notification_once(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    recipient_user_id: Uuid,
    event_date: NaiveDate,
    message: &CelebrationMessage,
    created_at: DateTime<Utc>,
) -> KabiPayResult<bool> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let notification_id = Uuid::new_v4();
    notification::ActiveModel {
        id: Set(notification_id),
        tenant_id: Set(tenant_id),
        user_id: Set(recipient_user_id),
        r#type: Set(Some(message.notification_type.into())),
        title: Set(Some(message.title.clone())),
        message: Set(Some(message.message.clone())),
        action_url: Set(Some("/notifications".into())),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(created_at),
        updated_at: Set(created_at),
    }
    .insert(&txn)
    .await
    .map_err(KabiPayError::from)?;

    let occurrence = txn
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO automated_notification_occurrence
                   (id, tenant_id, event_type, employee_id, event_date,
                    recipient_user_id, notification_id, created_at)
               VALUES ($1, $2, $3, $4, $5, $6, $7, $8)
               ON CONFLICT (tenant_id, event_type, employee_id, event_date, recipient_user_id)
               DO NOTHING
               RETURNING id"#,
            vec![
                Uuid::new_v4().into(),
                tenant_id.into(),
                message.notification_type.into(),
                employee_id.into(),
                event_date.into(),
                recipient_user_id.into(),
                notification_id.into(),
                created_at.into(),
            ],
        ))
        .await
        .map_err(KabiPayError::from)?;

    if occurrence.is_none() {
        txn.rollback().await.map_err(KabiPayError::from)?;
        return Ok(false);
    }
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(true)
}

fn event_message(
    kind: CelebrationEventKind,
    employee: &EligibleEmployee,
    business_date: NaiveDate,
    settings: &AutomationSettings,
) -> KabiPayResult<CelebrationMessage> {
    let service_years = (kind == CelebrationEventKind::WorkAnniversary)
        .then(|| completed_service_years(employee.date_of_joining, business_date))
        .flatten();
    let (title, body) = match kind {
        CelebrationEventKind::Birthday => (
            &settings.birthday_title_template,
            &settings.birthday_message_template,
        ),
        CelebrationEventKind::WorkAnniversary => (
            &settings.anniversary_title_template,
            &settings.anniversary_message_template,
        ),
    };
    render_event_message(kind, &employee.display_name(), service_years, title, body)
}

/// Generates due tenant-local employee celebrations exactly once per event and recipient.
pub async fn process_due_celebrations(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    business_date: NaiveDate,
    current_local_time: NaiveTime,
) -> KabiPayResult<CelebrationSweepResult> {
    let settings = load_automation_settings(db, tenant_id).await?;
    if !delivery_time_reached(current_local_time, settings.delivery_local_time) {
        return Ok(CelebrationSweepResult {
            before_delivery_time: true,
            ..Default::default()
        });
    }

    let employees = load_eligible_employees(db, tenant_id).await?;
    let company_users = if settings.company_sharing_enabled {
        load_company_notification_users(db, tenant_id).await?
    } else {
        Vec::new()
    };
    let mut result = CelebrationSweepResult::default();

    for employee in employees {
        let events = [
            (
                CelebrationEventKind::Birthday,
                settings.birthday_enabled,
                employee.date_of_birth,
                employee.share_birthday,
            ),
            (
                CelebrationEventKind::WorkAnniversary,
                settings.work_anniversary_enabled,
                Some(employee.date_of_joining),
                employee.share_work_anniversary,
            ),
        ];
        for (kind, enabled, source_date, consented) in events {
            let Some(_) = source_date.filter(|date| enabled && is_event_due(kind, *date, business_date)) else {
                continue;
            };
            result.eligible_events += 1;
            let message = event_message(kind, &employee, business_date, &settings)?;
            let recipients = select_recipient_user_ids(
                employee.employee_user_id,
                employee.manager_user_id,
                settings.company_sharing_enabled,
                consented,
                &company_users,
            );
            for recipient_user_id in recipients {
                if create_notification_once(
                    db,
                    tenant_id,
                    employee.employee_id,
                    recipient_user_id,
                    business_date,
                    &message,
                    Utc::now(),
                )
                .await?
                {
                    result.notifications_created += 1;
                } else {
                    result.duplicates_skipped += 1;
                }
            }
        }
    }

    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime};
    use uuid::Uuid;

    #[test]
    fn observes_february_29_birthdays_on_february_28_in_non_leap_years() {
        let birth = NaiveDate::from_ymd_opt(2000, 2, 29).expect("valid leap date");
        let observed = NaiveDate::from_ymd_opt(2026, 2, 28).expect("valid observation date");

        assert!(is_event_due(
            CelebrationEventKind::Birthday,
            birth,
            observed
        ));
    }

    #[test]
    fn does_not_observe_february_29_birthday_early_in_a_leap_year() {
        let birth = NaiveDate::from_ymd_opt(2000, 2, 29).expect("valid leap date");
        let early = NaiveDate::from_ymd_opt(2028, 2, 28).expect("valid date");

        assert!(!is_event_due(
            CelebrationEventKind::Birthday,
            birth,
            early
        ));
    }

    #[test]
    fn anniversary_requires_one_completed_year() {
        let joined = NaiveDate::from_ymd_opt(2026, 9, 8).expect("valid joining date");
        let today = NaiveDate::from_ymd_opt(2026, 9, 8).expect("valid business date");

        assert!(!is_event_due(
            CelebrationEventKind::WorkAnniversary,
            joined,
            today
        ));
    }

    #[test]
    fn returns_completed_service_years_only_on_or_after_first_anniversary() {
        let joined = NaiveDate::from_ymd_opt(2024, 9, 8).expect("valid joining date");
        let before = NaiveDate::from_ymd_opt(2025, 9, 7).expect("valid business date");
        let second = NaiveDate::from_ymd_opt(2026, 9, 8).expect("valid business date");

        assert_eq!(completed_service_years(joined, before), None);
        assert_eq!(completed_service_years(joined, second), Some(2));
    }

    #[test]
    fn birthday_template_rejects_age_birth_year_and_anniversary_tokens() {
        assert!(validate_template(
            "Happy {employee_name}",
            CelebrationEventKind::Birthday
        )
        .is_ok());
        assert!(validate_template("Age {age}", CelebrationEventKind::Birthday).is_err());
        assert!(validate_template("Born {birth_year}", CelebrationEventKind::Birthday).is_err());
        assert!(validate_template(
            "Years {service_years}",
            CelebrationEventKind::Birthday
        )
        .is_err());
    }

    #[test]
    fn anniversary_template_allows_service_years() {
        assert!(validate_template(
            "{employee_name}: {service_years} years",
            CelebrationEventKind::WorkAnniversary
        )
        .is_ok());
    }

    #[test]
    fn renders_safe_anniversary_message_from_validated_tokens() {
        let message = render_event_message(
            CelebrationEventKind::WorkAnniversary,
            " Anika Rao ",
            Some(3),
            "Work anniversary: {employee_name}",
            "Celebrating {employee_name}'s {service_years}-year work anniversary.",
        )
        .expect("valid anniversary message");

        assert_eq!(message.notification_type, "EMPLOYEE_WORK_ANNIVERSARY");
        assert_eq!(message.title, "Work anniversary: Anika Rao");
        assert_eq!(
            message.message,
            "Celebrating Anika Rao's 3-year work anniversary."
        );
    }

    #[test]
    fn anniversary_message_requires_completed_service_years() {
        let result = render_event_message(
            CelebrationEventKind::WorkAnniversary,
            "Anika Rao",
            None,
            "Work anniversary: {employee_name}",
            "{service_years} years",
        );

        assert!(result.is_err());
    }

    #[test]
    fn private_event_targets_employee_and_distinct_manager_only() {
        let employee = Uuid::new_v4();
        let manager = Uuid::new_v4();
        let company_users = [Uuid::new_v4(), Uuid::new_v4()];

        let recipients = select_recipient_user_ids(
            employee,
            Some(manager),
            false,
            true,
            &company_users,
        );

        assert_eq!(recipients, vec![employee, manager]);
        assert_eq!(
            select_recipient_user_ids(employee, Some(employee), false, false, &[]),
            vec![employee]
        );
    }

    #[test]
    fn company_event_requires_tenant_setting_and_employee_consent() {
        let employee = Uuid::new_v4();
        let manager = Uuid::new_v4();
        let company_users = [manager, employee, manager, Uuid::new_v4()];

        let private = select_recipient_user_ids(
            employee,
            Some(manager),
            true,
            false,
            &company_users,
        );
        let company = select_recipient_user_ids(
            employee,
            Some(manager),
            true,
            true,
            &company_users,
        );

        assert_eq!(private, vec![employee, manager]);
        assert_eq!(company.len(), 3);
        assert!(company.contains(&employee));
        assert!(company.contains(&manager));
    }

    #[test]
    fn generation_waits_until_configured_local_delivery_time() {
        let delivery = NaiveTime::from_hms_opt(9, 0, 0).expect("valid delivery time");
        let before = NaiveTime::from_hms_opt(8, 59, 59).expect("valid time");
        let at = NaiveTime::from_hms_opt(9, 0, 0).expect("valid time");

        assert!(!delivery_time_reached(before, delivery));
        assert!(delivery_time_reached(at, delivery));
    }
}
