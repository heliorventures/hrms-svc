use chrono::NaiveDate;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0010_time_shift_roster::{
    holiday, holiday_calendar, roster_slot,
};
use kabipay_db_entities::tenant::d0097_location_working_calendar::{
    employee_location_assignment, weekly_off_policy_version, working_calendar_profile,
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use super::{LocationVersion, PolicyVersion, WeeklyOffRule, WorkingCalendarSnapshot};
use crate::{KabiPayError, KabiPayResult};

pub async fn load_calendar<C: ConnectionTrait + Sync>(
    db: &C,
    tenant: Uuid,
    employee_ids: &[Uuid],
    from_date: NaiveDate,
    to_date: NaiveDate,
) -> KabiPayResult<WorkingCalendarSnapshot> {
    if from_date > to_date || (to_date - from_date).num_days() > 3660 {
        return Err(KabiPayError::Validation(
            "working calendar range must be ordered and no longer than ten years".into(),
        ));
    }
    let profile = working_calendar_profile::Entity::find_by_id(tenant)
        .one(db)
        .await?;
    let mut snapshot = WorkingCalendarSnapshot {
        activation_date: profile.as_ref().map(|row| row.activation_date),
        revision: profile.map(|row| row.revision),
        from_date,
        to_date,
        employees: Default::default(),
        assignments: Default::default(),
        policies: Default::default(),
        holidays: Default::default(),
        roster_work: Default::default(),
    };
    if employee_ids.is_empty() {
        return Ok(snapshot);
    }
    snapshot.employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant))
        .filter(employee::Column::Id.is_in(employee_ids.iter().copied()))
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect();
    if snapshot.employees.len()
        != employee_ids
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len()
    {
        return Err(KabiPayError::Validation(
            "working calendar employees must belong to this company".into(),
        ));
    }
    if snapshot
        .activation_date
        .is_none_or(|activation| to_date < activation)
    {
        return Ok(snapshot);
    }
    let assignments = employee_location_assignment::Entity::find()
        .filter(employee_location_assignment::Column::TenantId.eq(tenant))
        .filter(
            employee_location_assignment::Column::EmployeeId.is_in(employee_ids.iter().copied()),
        )
        .filter(employee_location_assignment::Column::EffectiveFrom.lte(to_date))
        .all(db)
        .await?;
    for row in assignments {
        snapshot
            .assignments
            .entry(row.employee_id)
            .or_default()
            .push(LocationVersion {
                id: row.id,
                location_id: row.location_id,
                effective_from: row.effective_from,
                revision: row.revision,
            });
    }
    let versions = weekly_off_policy_version::Entity::find()
        .filter(weekly_off_policy_version::Column::TenantId.eq(tenant))
        .filter(weekly_off_policy_version::Column::SupersededAt.is_null())
        .filter(weekly_off_policy_version::Column::EffectiveFrom.lte(to_date))
        .all(db)
        .await?;
    for row in versions {
        let rule = WeeklyOffRule {
            fixed_weekdays: serde_json::from_value(row.fixed_weekdays).map_err(|_| {
                KabiPayError::Internal("stored weekly-off weekdays are invalid".into())
            })?,
            saturday_ordinals: serde_json::from_value(row.saturday_ordinals).map_err(|_| {
                KabiPayError::Internal("stored Saturday selections are invalid".into())
            })?,
        };
        super::validate_rule(&rule)?;
        snapshot
            .policies
            .entry(row.location_id)
            .or_default()
            .push(PolicyVersion {
                id: row.id,
                effective_from: row.effective_from,
                inherits_default: row.inherits_default,
                rule,
            });
    }
    let calendars = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant))
        .all(db)
        .await?;
    if !calendars.is_empty() {
        let dates = holiday::Entity::find()
            .filter(holiday::Column::CalendarId.is_in(calendars.iter().map(|row| row.id)))
            .filter(holiday::Column::HolidayDate.gte(from_date))
            .filter(holiday::Column::HolidayDate.lte(to_date))
            .all(db)
            .await?;
        for date in dates {
            if let Some(calendar) = calendars.iter().find(|row| row.id == date.calendar_id) {
                snapshot
                    .holidays
                    .insert((date.holiday_date, calendar.location_id));
            }
        }
    }
    let roster = roster_slot::Entity::find()
        .filter(roster_slot::Column::TenantId.eq(tenant))
        .filter(roster_slot::Column::EmployeeId.is_in(employee_ids.iter().copied()))
        .filter(roster_slot::Column::SlotDate.gte(from_date))
        .filter(roster_slot::Column::SlotDate.lte(to_date))
        .all(db)
        .await?;
    snapshot.roster_work.extend(
        roster
            .into_iter()
            .map(|row| (row.employee_id, row.slot_date)),
    );
    let current_revision = working_calendar_profile::Entity::find_by_id(tenant)
        .one(db)
        .await?
        .map(|row| row.revision);
    if current_revision != snapshot.revision {
        return Err(KabiPayError::Validation(
            "working calendar changed while loading; reload and try again".into(),
        ));
    }
    Ok(snapshot)
}
