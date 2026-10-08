use chrono::{Duration, NaiveDate};
use kabipay_common::{working_calendar::load_calendar, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0010_time_shift_roster::{holiday, holiday_calendar},
    d0097_location_working_calendar::working_calendar_profile,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use uuid::Uuid;

pub async fn upcoming(
    db: &DatabaseConnection,
    tenant: Uuid,
    employee: Option<Uuid>,
    from: NaiveDate,
    limit: u64,
) -> KabiPayResult<Vec<(holiday::Model, String)>> {
    let profile = working_calendar_profile::Entity::find_by_id(tenant)
        .one(db)
        .await?;
    if profile.is_none() {
        return super::attendance_service::list_upcoming_holidays(db, tenant, from, limit).await;
    }
    let to = from
        .checked_add_signed(Duration::days(366))
        .ok_or_else(|| {
            KabiPayError::Validation("holiday date range exceeds supported dates".into())
        })?;
    let snapshot = if let Some(employee) = employee {
        Some(load_calendar(db, tenant, &[employee], from, to).await?)
    } else {
        None
    };
    let calendars = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant))
        .all(db)
        .await?;
    let calendars: std::collections::HashMap<_, _> = calendars
        .into_iter()
        .map(|calendar| (calendar.id, calendar))
        .collect();
    if calendars.is_empty() {
        return Ok(Vec::new());
    }
    let rows = holiday::Entity::find()
        .filter(holiday::Column::CalendarId.is_in(calendars.keys().copied()))
        .filter(holiday::Column::HolidayDate.between(from, to))
        .order_by_asc(holiday::Column::HolidayDate)
        .order_by_asc(holiday::Column::Id)
        .all(db)
        .await?;
    let mut result = Vec::new();
    for row in rows {
        let calendar = &calendars[&row.calendar_id];
        let day = employee
            .zip(snapshot.as_ref())
            .map(|(employee, snapshot)| snapshot.day(employee, row.holiday_date))
            .transpose()?;
        let active = profile
            .as_ref()
            .is_some_and(|p| row.holiday_date >= p.activation_date);
        if !active
            || calendar.location_id.is_none()
            || day.is_some_and(|day| calendar.location_id == day.location_id)
        {
            result.push((row, calendar.name.clone()));
        }
        if result.len() >= limit.clamp(1, 100) as usize {
            break;
        }
    }
    Ok(result)
}
