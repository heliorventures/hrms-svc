//! Tenant-scoped SeaORM queries and commands for shifts, holidays, and attendance.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use kabipay_common::tenant_business_clock::TenantBusinessClock;
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
    QuerySelect, Set, TransactionTrait,
};
use std::collections::HashMap;
use uuid::Uuid;

use crate::services::{
    attendance_regularization_service::{
        assert_locked_attendance_identity, insert_manual_segment, lock_employee_dates,
        update_manual_segment,
        validate_segment_with_connection, SegmentTimes,
        MANUAL_SELF_REPORTED,
    },
    timesheet_dates, timesheet_policy,
};
fn attendance_business_date_time(
    now_utc: DateTime<Utc>,
    clock: TenantBusinessClock,
) -> (NaiveDate, NaiveTime) {
    (clock.business_date(now_utc), clock.local_time(now_utc))
}

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

fn attendance_segment_minutes(row: &attendance::Model) -> i32 {
    if let (Some(check_in_at), Some(check_out_at)) = (row.check_in_at, row.check_out_at) {
        let seconds = check_out_at.signed_duration_since(check_in_at).num_seconds();
        return if seconds > 0 { (seconds / 60) as i32 } else { 0 };
    }
    match (row.check_in_time, row.check_out_time) {
        (Some(check_in), Some(check_out)) => segment_minutes(check_in, check_out),
        _ => 0,
    }
}

fn attendance_segment_seconds(row: &attendance::Model) -> i64 {
    if let (Some(check_in_at), Some(check_out_at)) = (row.check_in_at, row.check_out_at) {
        return check_out_at
            .signed_duration_since(check_in_at)
            .num_seconds()
            .max(0);
    }
    i64::from(attendance_segment_minutes(row)) * 60
}

fn assert_total_attendance_seconds_under_daily_cap(total_seconds: i64) -> KabiPayResult<()> {
    if total_seconds >= 24 * 60 * 60 {
        return Err(KabiPayError::Validation(
            "total attendance for a day must be less than 24 hours".into(),
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
    let total = segments.iter().map(attendance_segment_minutes).sum();
    let open_segment = segments
        .iter()
        .filter(|r| {
            r.status.as_deref() == Some("OPEN")
                && (r.check_in_at.is_some() || r.check_in_time.is_some())
                && r.check_out_at.is_none()
                && r.check_out_time.is_none()
        })
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
    clock: TenantBusinessClock,
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
    let (today, now_t) = attendance_business_date_time(now_ts, clock);
    let source = if geo.is_some() { "WEB+GPS" } else { "WEB" };
    let txn = db.begin().await?;
    // Retain missed punch-outs without inventing working time. The database permits
    // one OPEN row per employee, so retire earlier days before opening today's row.
    let stale = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.lt(today))
        .filter(attendance::Column::Status.eq("OPEN"))
        .all(&txn).await?;
    let mut dates: Vec<_> = stale.iter().map(|row| row.work_date).collect();
    dates.push(today);
    lock_employee_dates(&txn, tenant_id, employee_id, &dates).await?;
    attendance::Entity::update_many()
        .col_expr(attendance::Column::Status, sea_orm::sea_query::Expr::value("INCOMPLETE"))
        .col_expr(attendance::Column::UpdatedAt, sea_orm::sea_query::Expr::value(now_ts))
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.lt(today))
        .filter(attendance::Column::Status.eq("OPEN"))
        .exec(&txn).await?;
    let open = open_punch_on_date(tenant_id, employee_id, today)
        .one(&txn)
        .await?;

    if let Some(row) = open {
        let existing = attendance::Entity::find()
            .filter(attendance::Column::TenantId.eq(tenant_id))
            .filter(attendance::Column::EmployeeId.eq(employee_id))
            .filter(attendance::Column::WorkDate.eq(row.work_date))
            .all(&txn)
            .await?;
        let open_seconds = row
            .check_in_at
            .map(|check_in_at| now_ts.signed_duration_since(check_in_at).num_seconds())
            .or_else(|| {
                row.check_in_time
                    .map(|check_in| i64::from(segment_minutes(check_in, now_t)) * 60)
            })
            .unwrap_or_default();
        let completed_seconds: i64 = existing
            .iter()
            .filter(|segment| segment.id != row.id)
            .map(attendance_segment_seconds)
            .sum();
        assert_total_attendance_seconds_under_daily_cap(completed_seconds + open_seconds)?;
        let id = row.id;
        let mut am: attendance::ActiveModel = row.into();
        am.check_out_time = Set(Some(now_t));
        am.check_out_at = Set(Some(now_ts));
        am.status = Set(Some("COMPLETE".into()));
        if let Some(g) = geo {
            am.check_out_lat = Set(Some(g.lat));
            am.check_out_lng = Set(Some(g.lng));
        }
        am.source = Set(Some(source.into()));
        am.updated_at = Set(now_ts);
        am.update(&txn).await?;
        txn.commit().await?;
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
        check_in_at: Set(Some(now_ts)),
        check_out_at: Set(None),
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
    am.insert(&txn).await?;
    txn.commit().await?;
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
    clock: TenantBusinessClock,
    work_date: NaiveDate,
    check_in_time: NaiveTime,
    check_out_time: NaiveTime,
) -> KabiPayResult<attendance::Model> {
    let segment = SegmentTimes::for_manual_input(work_date, check_in_time, check_out_time);
    let instants = segment.to_instants(clock)?;
    let today = clock.now_date();
    let txn = db.begin().await?;
    lock_employee_dates(&txn, tenant_id, employee_id, &[work_date]).await?;
    validate_segment_with_connection(
        &txn,
        tenant_id,
        employee_id,
        segment,
        None,
        false,
        today,
    )
    .await?;
    let created = insert_manual_segment(
        &txn,
        tenant_id,
        employee_id,
        segment,
        instants,
        MANUAL_SELF_REPORTED,
        Utc::now(),
    )
    .await?;
    txn.commit().await?;
    Ok(created)
}

fn open_punch_on_date(tenant_id: Uuid, employee_id: Uuid, work_date: NaiveDate) -> sea_orm::Select<attendance::Entity> {
    attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.eq(work_date))
        .filter(attendance::Column::Status.eq("OPEN"))
        .filter(attendance::Column::CheckInTime.is_not_null())
        .filter(attendance::Column::CheckOutTime.is_null())
        .order_by_desc(attendance::Column::CreatedAt)
}

pub async fn update_manual_attendance_segment(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    attendance_id: Uuid,
    requesting_employee_id: Uuid,
    clock: TenantBusinessClock,
    work_date: NaiveDate,
    check_in_time: NaiveTime,
    check_out_time: NaiveTime,
) -> KabiPayResult<attendance::Model> {
    let txn = db.begin().await?;
    let row = attendance::Entity::find_by_id(attendance_id)
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "attendance",
            id: attendance_id.to_string(),
        })?;

    if row.employee_id != requesting_employee_id {
        return Err(KabiPayError::Forbidden(
            "attendance segment belongs to another employee".into(),
        ));
    }

    let segment = SegmentTimes::for_manual_input(work_date, check_in_time, check_out_time);
    let instants = segment.to_instants(clock)?;
    let today = clock.now_date();
    let locked_employee_id = row.employee_id;
    let locked_work_date = row.work_date;
    lock_employee_dates(
        &txn,
        tenant_id,
        locked_employee_id,
        &[locked_work_date, work_date],
    )
    .await?;
    let row = attendance::Entity::find_by_id(attendance_id)
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "attendance",
            id: attendance_id.to_string(),
        })?;
    assert_locked_attendance_identity(
        locked_employee_id,
        locked_work_date,
        row.employee_id,
        row.work_date,
    )?;
    validate_segment_with_connection(
        &txn,
        tenant_id,
        row.employee_id,
        segment,
        Some(attendance_id),
        false,
        today,
    )
    .await?;
    let updated = update_manual_segment(
        &txn,
        row,
        segment,
        instants,
        MANUAL_SELF_REPORTED,
        Utc::now(),
    )
    .await?;
    txn.commit().await?;
    Ok(updated)
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
    assert_timesheet_hours_precision(hours_worked)?;
    if hours_worked <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "hoursWorked must be greater than zero".into(),
        ));
    }
    timesheet_policy::assert_required_project_and_task(
        project_code.as_deref(),
        description.as_deref(),
    )?;
    timesheet_policy::assert_work_date_allowed_for_entry(db, tenant_id, work_date).await?;
    timesheet_policy::assert_week_has_no_active_submission(
        db,
        tenant_id,
        employee_id,
        work_date,
    )
    .await?;
    timesheet_policy::assert_day_hours_with_entry_change(
        db,
        tenant_id,
        employee_id,
        work_date,
        None,
        hours_worked,
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
    assert_timesheet_hours_precision(hours_worked)?;
    if hours_worked <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "hoursWorked must be greater than zero".into(),
        ));
    }
    timesheet_policy::assert_required_project_and_task(
        project_code.as_deref(),
        description.as_deref(),
    )?;
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
    let row_status = row.status.trim().to_uppercase();
    let existing_week_start = timesheet_dates::week_monday_sunday(row.work_date).0;
    let next_week_start = timesheet_dates::week_monday_sunday(work_date).0;
    timesheet_policy::assert_entry_mut_allowed(db, tenant_id, &row).await?;
    timesheet_policy::assert_work_date_allowed_for_entry(db, tenant_id, work_date).await?;
    if row_status == "APPROVED" {
        if existing_week_start != next_week_start {
            return Err(KabiPayError::Validation(
                "approved timesheet rows cannot be moved to another week".into(),
            ));
        }
    } else {
        timesheet_policy::assert_week_has_no_active_submission(
            db,
            tenant_id,
            employee_id,
            work_date,
        )
        .await?;
    }
    timesheet_policy::assert_day_hours_with_entry_change(
        db,
        tenant_id,
        employee_id,
        work_date,
        Some(entry_id),
        hours_worked,
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
    let value = s.trim();
    let decimal_places = value
        .split_once('.')
        .map(|(_, fraction)| fraction.len())
        .unwrap_or(0);
    if decimal_places > 2 {
        return Err(KabiPayError::Validation(
            "hours worked supports at most two decimal places".into(),
        ));
    }
    let parsed = Decimal::from_str(value)
        .map_err(|_| KabiPayError::Validation("invalid hoursWorked; use a decimal string".into()))?;
    assert_timesheet_hours_precision(parsed)?;
    Ok(parsed)
}

fn assert_timesheet_hours_precision(hours: Decimal) -> KabiPayResult<()> {
    if hours.normalize().scale() > 2 {
        return Err(KabiPayError::Validation(
            "hours worked supports at most two decimal places".into(),
        ));
    }
    Ok(())
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

#[cfg(test)]
mod tests {
    #[test]
    fn live_punch_only_considers_the_current_tenant_day() {
        use sea_orm::QueryTrait;
        let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
        let before = "2026-09-05T18:29:59Z".parse().unwrap();
        let after = "2026-09-05T18:30:00Z".parse().unwrap();
        assert_ne!(clock.business_date(before), clock.business_date(after));
        let sql = open_punch_on_date(Uuid::nil(), Uuid::nil(), clock.business_date(after))
            .build(sea_orm::DbBackend::Postgres).to_string();
        assert!(sql.contains("\"work_date\" = '2026-09-06'"), "{sql}");
        assert!(sql.contains("\"status\" = 'OPEN'"));
    }
    use super::*;

    #[test]
    fn timesheet_hours_reject_more_than_two_decimal_places() {
        assert!(parse_hours("1").is_ok());
        assert!(parse_hours("1.2").is_ok());
        assert!(parse_hours("1.23").is_ok());
        assert!(parse_hours("1.234").is_err());
    }
    use chrono::{NaiveDate, NaiveTime};

    #[test]
    fn live_punch_clock_uses_configured_business_timezone_not_utc_wall_time() {
        let now_utc = "2026-08-22T20:00:00Z"
            .parse::<DateTime<Utc>>()
            .expect("valid UTC timestamp");

        let clock = TenantBusinessClock::from_name("Asia/Kolkata").expect("valid timezone");
        let (work_date, punch_time) = attendance_business_date_time(now_utc, clock);

        assert_eq!(
            work_date,
            NaiveDate::from_ymd_opt(2026, 8, 23).expect("valid date")
        );
        assert_eq!(
            punch_time,
            NaiveTime::from_hms_opt(1, 30, 0).expect("valid time")
        );
    }

    #[test]
    fn attendance_daily_total_must_remain_below_twenty_four_hours() {
        assert!(
            crate::services::attendance_regularization_service::assert_total_attendance_minutes_under_daily_cap(
                23 * 60 + 59,
            )
            .is_ok()
        );
        assert!(
            crate::services::attendance_regularization_service::assert_total_attendance_minutes_under_daily_cap(
                24 * 60,
            )
            .is_err()
        );
        assert!(
            crate::services::attendance_regularization_service::assert_total_attendance_minutes_under_daily_cap(
                24 * 60 + 1,
            )
            .is_err()
        );
        assert!(assert_total_attendance_seconds_under_daily_cap(24 * 60 * 60 - 1).is_ok());
        assert!(assert_total_attendance_seconds_under_daily_cap(24 * 60 * 60).is_err());
    }
}
