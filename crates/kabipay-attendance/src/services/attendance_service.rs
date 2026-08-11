//! Tenant-scoped SeaORM queries and commands for shifts, holidays, and attendance.

use chrono::{NaiveDate, NaiveTime, Utc};
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use std::str::FromStr;

/// WGS84 coordinates for a punch; both axes must be set when used.
pub struct PunchGeo {
    pub lat: Decimal,
    pub lng: Decimal,
}
use kabipay_db_entities::tenant::d0010_time_shift_roster::{
    attendance, holiday, holiday_calendar, shift, timesheet_entry,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::services::{hrms_master_service, timesheet_policy};

const ATTENDANCE_STATUS_COMPLETE: &str = "COMPLETE";
const MANUAL_ATTENDANCE_SOURCE: &str = "WEB+MANUAL";
const MANUAL_SELF_REPORTED: &str = "SELF_REPORTED";
const MANUAL_REGULARIZED: &str = "REGULARIZED";
const MAX_DAY_MINUTES: i32 = 24 * 60;

pub async fn list_shifts(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<shift::Model>> {
    let limit = limit.clamp(1, 200);
    shift::Entity::find()
        .filter(shift::Column::TenantId.eq(tenant_id))
        .order_by_asc(shift::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_attendance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    scope_filter: &EmployeeScopeFilter,
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
) -> KabiPayResult<Vec<attendance::Model>> {
    let limit = limit.clamp(1, 1000);
    match scope_filter {
        EmployeeScopeFilter::Empty => return Ok(vec![]),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(vec![]),
        _ => {}
    }
    let mut q = attendance::Entity::find().filter(attendance::Column::TenantId.eq(tenant_id));
    if let EmployeeScopeFilter::EmployeeIds(ids) = scope_filter {
        q = q.filter(attendance::Column::EmployeeId.is_in(ids.clone()));
    }
    if let Some(fd) = from_date {
        q = q.filter(attendance::Column::WorkDate.gte(fd));
    }
    if let Some(td) = to_date {
        q = q.filter(attendance::Column::WorkDate.lte(td));
    }
    q.order_by_desc(attendance::Column::WorkDate)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Public holidays on or after `from`, ordered by date (tenant-wide: all
/// holiday calendars in the schema).
pub async fn list_upcoming_holidays(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    from: NaiveDate,
    limit: u64,
) -> KabiPayResult<Vec<(holiday::Model, String)>> {
    let limit = limit.clamp(1, 100);
    let cals = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .all(db)
        .await?;
    if cals.is_empty() {
        return Ok(vec![]);
    }
    let names: HashMap<Uuid, String> = cals.iter().map(|c| (c.id, c.name.clone())).collect();
    let cal_ids: Vec<Uuid> = cals.iter().map(|c| c.id).collect();
    let rows = holiday::Entity::find()
        .filter(holiday::Column::CalendarId.is_in(cal_ids))
        .filter(holiday::Column::HolidayDate.gte(from))
        .order_by_asc(holiday::Column::HolidayDate)
        .limit(limit)
        .all(db)
        .await?;
    let out: Vec<(holiday::Model, String)> = rows
        .into_iter()
        .filter_map(|h| names.get(&h.calendar_id).cloned().map(|n| (h, n)))
        .collect();
    Ok(out)
}

/// Minutes in a single completed in→out pair (same calendar work_date).
fn segment_minutes(t_in: chrono::NaiveTime, t_out: chrono::NaiveTime) -> i32 {
    use chrono::Timelike;
    let s_in = t_in.num_seconds_from_midnight() as i64;
    let s_out = t_out.num_seconds_from_midnight() as i64;
    let d = s_out - s_in;
    if d <= 0 {
        return 0;
    }
    (d / 60) as i32
}

fn manual_segment_minutes(
    check_in_time: NaiveTime,
    check_out_time: NaiveTime,
) -> KabiPayResult<i32> {
    if check_in_time >= check_out_time {
        return Err(KabiPayError::Validation(
            "checkInTime must be before checkOutTime (same-day segment only)".into(),
        ));
    }
    Ok(segment_minutes(check_in_time, check_out_time))
}

async fn assert_manual_attendance_segment_allowed(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
    check_in_time: NaiveTime,
    check_out_time: NaiveTime,
    excluded_attendance_id: Option<Uuid>,
    privileged_regularize: bool,
) -> KabiPayResult<()> {
    let today = Utc::now().date_naive();
    if work_date > today {
        return Err(KabiPayError::Validation(
            "workDate cannot be in the future".into(),
        ));
    }

    let requested_minutes = manual_segment_minutes(check_in_time, check_out_time)?;
    let policy = hrms_master_service::load_attendance_adjustment_policy(db, tenant_id).await?;
    let days_since = today.signed_duration_since(work_date).num_days();
    let window = policy.max_self_adjust_days.max(0);
    if days_since > window && !privileged_regularize {
        return Err(KabiPayError::Forbidden(format!(
            "manual attendance is limited to the last {} calendar days unless you hold attendance regularization permission",
            window
        )));
    }

    let existing = list_employee_attendance_on_date(db, tenant_id, employee_id, work_date).await?;
    let mut total_minutes = requested_minutes;
    for row in existing {
        if excluded_attendance_id == Some(row.id) {
            continue;
        }
        match (row.check_in_time, row.check_out_time) {
            (Some(existing_in), Some(existing_out)) => {
                if check_in_time < existing_out && check_out_time > existing_in {
                    return Err(KabiPayError::Validation(
                        "manual attendance overlaps with an existing segment for this day".into(),
                    ));
                }
                total_minutes += segment_minutes(existing_in, existing_out);
            }
            (Some(_), None) => {
                return Err(KabiPayError::Validation(
                    "complete the open punch before adjusting manual attendance for this day"
                        .into(),
                ));
            }
            _ => {}
        }
    }

    if total_minutes > MAX_DAY_MINUTES {
        return Err(KabiPayError::Validation(
            "total attendance for a day cannot exceed 24 hours".into(),
        ));
    }

    Ok(())
}

/// All attendance rows (segments) for one employee on one work day, ordered oldest first.
pub async fn list_employee_attendance_on_date(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
) -> KabiPayResult<Vec<attendance::Model>> {
    attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.eq(work_date))
        .order_by_asc(attendance::Column::CreatedAt)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Aggregated stats for a day: sum of (check-out − check-in) for every completed
/// segment, plus the current open segment (checked in, not out) if any.
pub struct PunchDaySummary {
    pub work_date: NaiveDate,
    pub total_worked_minutes: i32,
    pub open_segment: Option<attendance::Model>,
    pub segments: Vec<attendance::Model>,
}

pub async fn punch_day_summary(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
) -> KabiPayResult<PunchDaySummary> {
    let segments = list_employee_attendance_on_date(db, tenant_id, employee_id, work_date).await?;
    let mut total = 0i32;
    for row in &segments {
        if let (Some(tin), Some(tout)) = (row.check_in_time, row.check_out_time) {
            total += segment_minutes(tin, tout);
        }
    }
    let open_segment = segments
        .iter()
        .filter(|r| r.check_in_time.is_some() && r.check_out_time.is_none())
        .max_by_key(|r| r.created_at)
        .cloned();
    Ok(PunchDaySummary {
        work_date,
        total_worked_minutes: total,
        open_segment,
        segments,
    })
}

/// **Multi-segment punch:** each pair (punch in → punch out) is a separate `attendance` row
/// for the same `work_date`. The next call after a completed segment starts a new segment
/// (new check-in row). `total` worked time for the day is the sum of all completed segments.
///
/// `geo` applies to **this** event: on punch-in (new row) it fills `check_in_*`;
/// on punch-out (update open row) it fills `check_out_*`. Columns in Liquibase: `attendance`
/// already has `check_in_lat` / `check_in_lng` / `check_out_lat` / `check_out_lng`.
pub async fn punch_today(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    geo: Option<PunchGeo>,
    client_ip: Option<&str>,
) -> KabiPayResult<attendance::Model> {
    let policy = crate::services::punch_policy::find_punch_policy(db, tenant_id).await?;
    let lat_lng = geo.as_ref().map(|g| (g.lat, g.lng));
    crate::services::punch_policy::validate_live_punch_for_policy(
        policy.as_ref(),
        lat_lng,
        client_ip,
    )?;

    let now_ts = Utc::now();
    let today = now_ts.date_naive();
    let now_t = now_ts.naive_utc().time();
    let source = if geo.is_some() { "WEB+GPS" } else { "WEB" };
    let open = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.eq(today))
        .filter(attendance::Column::CheckInTime.is_not_null())
        .filter(attendance::Column::CheckOutTime.is_null())
        .order_by_desc(attendance::Column::CreatedAt)
        .one(db)
        .await?;

    if let Some(row) = open {
        let id = row.id;
        let mut am: attendance::ActiveModel = row.into();
        am.check_out_time = Set(Some(now_t));
        am.status = Set(Some("COMPLETE".into()));
        if let Some(g) = geo {
            am.check_out_lat = Set(Some(g.lat));
            am.check_out_lng = Set(Some(g.lng));
        }
        am.source = Set(Some(source.into()));
        am.updated_at = Set(now_ts);
        am.update(db).await?;
        return attendance::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("attendance row missing after update".into()));
    }

    let id = Uuid::new_v4();
    let (in_lat, in_lng) = match &geo {
        Some(g) => (Some(g.lat), Some(g.lng)),
        None => (None, None),
    };
    let am = attendance::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        shift_id: Set(None),
        work_date: Set(today),
        check_in_time: Set(Some(now_t)),
        check_out_time: Set(None),
        check_in_lat: Set(in_lat),
        check_in_lng: Set(in_lng),
        check_out_lat: Set(None),
        check_out_lng: Set(None),
        source: Set(Some(source.into())),
        status: Set(Some("OPEN".into())),
        regularization_status: Set(None),
        biometric_ref: Set(None),
        overtime_hours: Set(None),
        late_minutes: Set(None),
        early_exit_minutes: Set(None),
        created_at: Set(now_ts),
        updated_at: Set(now_ts),
    };
    am.insert(db).await?;
    attendance::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("attendance row missing after insert".into()))
}

/// One completed in→out **segment** for a chosen `work_date` when the user missed live punches
/// (e.g. forgot to open the app). **Same calendar day** only — night shifts that span midnight
/// are not represented as a single row here. Stored with `source` `WEB+MANUAL` and
/// `regularization_status` `SELF_REPORTED` for audit.
pub async fn add_manual_attendance_segment(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
    check_in_time: NaiveTime,
    check_out_time: NaiveTime,
    privileged_regularize: bool,
) -> KabiPayResult<attendance::Model> {
    assert_manual_attendance_segment_allowed(
        db,
        tenant_id,
        employee_id,
        work_date,
        check_in_time,
        check_out_time,
        None,
        privileged_regularize,
    )
    .await?;
    let now_ts = Utc::now();
    let id = Uuid::new_v4();
    let am = attendance::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        shift_id: Set(None),
        work_date: Set(work_date),
        check_in_time: Set(Some(check_in_time)),
        check_out_time: Set(Some(check_out_time)),
        check_in_lat: Set(None),
        check_in_lng: Set(None),
        check_out_lat: Set(None),
        check_out_lng: Set(None),
        source: Set(Some(MANUAL_ATTENDANCE_SOURCE.into())),
        status: Set(Some(ATTENDANCE_STATUS_COMPLETE.into())),
        regularization_status: Set(Some(MANUAL_SELF_REPORTED.into())),
        biometric_ref: Set(None),
        overtime_hours: Set(None),
        late_minutes: Set(None),
        early_exit_minutes: Set(None),
        created_at: Set(now_ts),
        updated_at: Set(now_ts),
    };
    am.insert(db).await?;
    attendance::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted attendance row not found".into()))
}

pub async fn update_manual_attendance_segment(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    attendance_id: Uuid,
    requesting_employee_id: Uuid,
    work_date: NaiveDate,
    check_in_time: NaiveTime,
    check_out_time: NaiveTime,
    privileged_regularize: bool,
) -> KabiPayResult<attendance::Model> {
    let row = attendance::Entity::find_by_id(attendance_id)
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "attendance",
            id: attendance_id.to_string(),
        })?;

    if row.employee_id != requesting_employee_id && !privileged_regularize {
        return Err(KabiPayError::Forbidden(
            "attendance segment belongs to another employee".into(),
        ));
    }

    assert_manual_attendance_segment_allowed(
        db,
        tenant_id,
        row.employee_id,
        work_date,
        check_in_time,
        check_out_time,
        Some(attendance_id),
        privileged_regularize,
    )
    .await?;

    let mut am: attendance::ActiveModel = row.into();
    am.work_date = Set(work_date);
    am.check_in_time = Set(Some(check_in_time));
    am.check_out_time = Set(Some(check_out_time));
    am.check_in_lat = Set(None);
    am.check_in_lng = Set(None);
    am.check_out_lat = Set(None);
    am.check_out_lng = Set(None);
    am.source = Set(Some(MANUAL_ATTENDANCE_SOURCE.into()));
    am.status = Set(Some(ATTENDANCE_STATUS_COMPLETE.into()));
    am.regularization_status = Set(Some(
        if privileged_regularize {
            MANUAL_REGULARIZED
        } else {
            MANUAL_SELF_REPORTED
        }
        .into(),
    ));
    am.updated_at = Set(Utc::now());
    am.update(db).await?;

    attendance::Entity::find_by_id(attendance_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated attendance row not found".into()))
}

pub async fn list_timesheet_entries(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    limit: u64,
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
) -> KabiPayResult<Vec<timesheet_entry::Model>> {
    let limit = limit.clamp(1, 500);
    let mut q = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false));
    if let Some(fd) = from_date {
        q = q.filter(timesheet_entry::Column::WorkDate.gte(fd));
    }
    if let Some(td) = to_date {
        q = q.filter(timesheet_entry::Column::WorkDate.lte(td));
    }
    q.order_by_desc(timesheet_entry::Column::WorkDate)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn create_timesheet_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    work_date: NaiveDate,
    hours_worked: Decimal,
    project_code: Option<String>,
    description: Option<String>,
) -> KabiPayResult<timesheet_entry::Model> {
    if hours_worked <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "hoursWorked must be greater than zero".into(),
        ));
    }
    timesheet_policy::assert_work_date_allowed_for_entry(db, tenant_id, work_date).await?;
    timesheet_policy::assert_week_has_no_active_submission(
        db,
        tenant_id,
        employee_id,
        work_date,
    )
    .await?;
    timesheet_policy::assert_week_hours_with_entry_change(
        db,
        tenant_id,
        employee_id,
        work_date,
        None,
        hours_worked,
    )
    .await?;
    crate::services::timesheet_project_assignment_service::assert_project_allowed_for_employee(
        db,
        tenant_id,
        employee_id,
        project_code.as_deref(),
    )
    .await?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    let am = timesheet_entry::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        work_date: Set(work_date),
        hours_worked: Set(hours_worked),
        project_code: Set(project_code),
        description: Set(description),
        status: Set("DRAFT".into()),
        batch_id: Set(None),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    timesheet_entry::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted timesheet_entry not found".into()))
}

pub async fn delete_timesheet_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    entry_id: Uuid,
) -> KabiPayResult<bool> {
    let row = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::Id.eq(entry_id))
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "timesheet_entry",
            id: entry_id.to_string(),
        })?;
    if row.employee_id != employee_id {
        return Err(KabiPayError::Forbidden(
            "timesheet entry belongs to another employee".into(),
        ));
    }
    if row.is_deleted {
        return Ok(false);
    }
    timesheet_policy::assert_entry_mut_allowed(db, tenant_id, &row).await?;
    let st = row.status.trim().to_uppercase();
    if st != "DRAFT" {
        return Err(KabiPayError::Validation(
            "only draft timesheet rows can be deleted".into(),
        ));
    }
    let mut am: timesheet_entry::ActiveModel = row.into();
    am.is_deleted = Set(true);
    am.deleted_at = Set(Some(Utc::now()));
    am.updated_at = Set(Utc::now());
    am.update(db).await?;
    Ok(true)
}

pub async fn update_timesheet_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    entry_id: Uuid,
    work_date: NaiveDate,
    hours_worked: Decimal,
    project_code: Option<String>,
    description: Option<String>,
) -> KabiPayResult<timesheet_entry::Model> {
    if hours_worked <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "hoursWorked must be greater than zero".into(),
        ));
    }
    let row = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::Id.eq(entry_id))
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "timesheet_entry",
            id: entry_id.to_string(),
        })?;
    if row.employee_id != employee_id {
        return Err(KabiPayError::Forbidden(
            "timesheet entry belongs to another employee".into(),
        ));
    }
    timesheet_policy::assert_entry_mut_allowed(db, tenant_id, &row).await?;
    timesheet_policy::assert_work_date_allowed_for_entry(db, tenant_id, work_date).await?;
    timesheet_policy::assert_week_has_no_active_submission(
        db,
        tenant_id,
        employee_id,
        work_date,
    )
    .await?;
    timesheet_policy::assert_week_hours_with_entry_change(
        db,
        tenant_id,
        employee_id,
        work_date,
        Some(entry_id),
        hours_worked,
    )
    .await?;
    crate::services::timesheet_project_assignment_service::assert_project_allowed_for_employee(
        db,
        tenant_id,
        employee_id,
        project_code.as_deref(),
    )
    .await?;
    let now = Utc::now();
    let mut am: timesheet_entry::ActiveModel = row.into();
    am.work_date = Set(work_date);
    am.hours_worked = Set(hours_worked);
    am.project_code = Set(project_code);
    am.description = Set(description);
    am.updated_at = Set(now);
    am.update(db).await?;
    timesheet_entry::Entity::find_by_id(entry_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("timesheet_entry missing after update".into()))
}

pub fn parse_hours(s: &str) -> KabiPayResult<Decimal> {
    Decimal::from_str(s.trim())
        .map_err(|_| KabiPayError::Validation("invalid hoursWorked; use a decimal string".into()))
}

/// Validates optional WGS84 pair; `latitude` and `longitude` must both be set or both omitted.
pub fn parse_punch_geo(
    latitude: Option<f64>,
    longitude: Option<f64>,
) -> KabiPayResult<Option<PunchGeo>> {
    match (latitude, longitude) {
        (None, None) => Ok(None),
        (Some(lat), Some(lon)) => {
            if !(-90.0..=90.0).contains(&lat) {
                return Err(KabiPayError::Validation(
                    "latitude must be between -90 and 90".into(),
                ));
            }
            if !(-180.0..=180.0).contains(&lon) {
                return Err(KabiPayError::Validation(
                    "longitude must be between -180 and 180".into(),
                ));
            }
            let lat_d = Decimal::from_str(&format!("{lat:.7}"))
                .map_err(|_| KabiPayError::Validation("invalid latitude encoding".into()))?;
            let lng_d = Decimal::from_str(&format!("{lon:.7}"))
                .map_err(|_| KabiPayError::Validation("invalid longitude encoding".into()))?;
            Ok(Some(PunchGeo {
                lat: lat_d,
                lng: lng_d,
            }))
        }
        _ => Err(KabiPayError::Validation(
            "pass both latitude and longitude, or neither".into(),
        )),
    }
}

// --- Holiday calendar admin (tenant leave / attendance planning) ---

pub async fn list_holiday_calendars(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    year: Option<i32>,
    limit: u64,
) -> KabiPayResult<Vec<holiday_calendar::Model>> {
    let limit = limit.clamp(1, 200);
    let mut q = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id));
    if let Some(y) = year {
        q = q.filter(holiday_calendar::Column::Year.eq(y));
    }
    q.order_by_asc(holiday_calendar::Column::Year)
        .order_by_asc(holiday_calendar::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn upsert_holiday_calendar(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    year: i32,
    location_id: Option<Uuid>,
) -> KabiPayResult<holiday_calendar::Model> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(KabiPayError::Validation(
            "holiday calendar name is required".into(),
        ));
    }
    let now = Utc::now();
    if let Some(cid) = id {
        let row = holiday_calendar::Entity::find_by_id(cid)
            .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "holiday_calendar",
                id: cid.to_string(),
            })?;
        let mut am: holiday_calendar::ActiveModel = row.into();
        am.name = Set(name);
        am.year = Set(year);
        am.location_id = Set(location_id);
        am.updated_at = Set(now);
        return Ok(am.update(db).await?);
    }
    let new_id = Uuid::new_v4();
    let am = holiday_calendar::ActiveModel {
        id: Set(new_id),
        tenant_id: Set(tenant_id),
        location_id: Set(location_id),
        name: Set(name),
        year: Set(year),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    holiday_calendar::Entity::find_by_id(new_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted holiday_calendar not found".into()))
}

pub async fn delete_holiday_calendar(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    calendar_id: Uuid,
) -> KabiPayResult<u64> {
    let n = holiday_calendar::Entity::delete_many()
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .filter(holiday_calendar::Column::Id.eq(calendar_id))
        .exec(db)
        .await?
        .rows_affected;
    Ok(n)
}

async fn assert_calendar_tenant(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    calendar_id: Uuid,
) -> KabiPayResult<holiday_calendar::Model> {
    holiday_calendar::Entity::find_by_id(calendar_id)
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "holiday_calendar",
            id: calendar_id.to_string(),
        })
}

pub async fn upsert_holiday_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    calendar_id: Uuid,
    id: Option<Uuid>,
    holiday_date: NaiveDate,
    name: String,
    holiday_type: Option<String>,
) -> KabiPayResult<holiday::Model> {
    assert_calendar_tenant(db, tenant_id, calendar_id).await?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(KabiPayError::Validation("holiday name is required".into()));
    }
    let now = Utc::now();
    if let Some(hid) = id {
        let row = holiday::Entity::find_by_id(hid)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "holiday",
                id: hid.to_string(),
            })?;
        if row.calendar_id != calendar_id {
            return Err(KabiPayError::Validation(
                "holiday does not belong to this calendar".into(),
            ));
        }
        let mut am: holiday::ActiveModel = row.into();
        am.holiday_date = Set(holiday_date);
        am.name = Set(name);
        am.r#type = Set(holiday_type);
        am.updated_at = Set(now);
        return Ok(am.update(db).await?);
    }
    let new_id = Uuid::new_v4();
    let am = holiday::ActiveModel {
        id: Set(new_id),
        calendar_id: Set(calendar_id),
        holiday_date: Set(holiday_date),
        name: Set(name),
        r#type: Set(holiday_type),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    holiday::Entity::find_by_id(new_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted holiday not found".into()))
}

pub async fn delete_holiday_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    holiday_id: Uuid,
) -> KabiPayResult<u64> {
    let row = holiday::Entity::find_by_id(holiday_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "holiday",
            id: holiday_id.to_string(),
        })?;
    assert_calendar_tenant(db, tenant_id, row.calendar_id).await?;
    let n = holiday::Entity::delete_many()
        .filter(holiday::Column::Id.eq(holiday_id))
        .exec(db)
        .await?
        .rows_affected;
    Ok(n)
}

pub async fn list_holidays_in_calendar(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    calendar_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<holiday::Model>> {
    assert_calendar_tenant(db, tenant_id, calendar_id).await?;
    let limit = limit.clamp(1, 500);
    Ok(holiday::Entity::find()
        .filter(holiday::Column::CalendarId.eq(calendar_id))
        .order_by_asc(holiday::Column::HolidayDate)
        .limit(limit)
        .all(db)
        .await?)
}
