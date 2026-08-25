//! Policy-derived, scope-filtered attendance reporting.

use std::collections::{HashMap, HashSet};

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Datelike, Days, Duration, NaiveDate, Timelike, Utc, Weekday};
use kabipay_common::{
    client_data_scope::EmployeeScopeFilter, tenant_business_clock::TenantBusinessClock,
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0010_time_shift_roster::{
        attendance, employee_shift, holiday, holiday_calendar, roster_slot, shift,
    },
    d0011_leave::leave_request,
    d0017_onboarding_offboarding::separation,
};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use uuid::Uuid;

const DEFAULT_PAGE_SIZE: usize = 50;
const MAX_PAGE_SIZE: usize = 100;
const MAX_REPORT_DAYS: i64 = 92;

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum AttendanceDayStatus {
    Present,
    HalfDay,
    Absent,
    OnLeave,
    Holiday,
    WeeklyOff,
    Incomplete,
    Unscheduled,
}

impl AttendanceDayStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Present => "PRESENT",
            Self::HalfDay => "HALF_DAY",
            Self::Absent => "ABSENT",
            Self::OnLeave => "ON_LEAVE",
            Self::Holiday => "HOLIDAY",
            Self::WeeklyOff => "WEEKLY_OFF",
            Self::Incomplete => "INCOMPLETE",
            Self::Unscheduled => "UNSCHEDULED",
        }
    }
}

pub fn classify_day(
    expected_minutes: Option<i32>,
    logged_minutes: i32,
    has_open_segment: bool,
    on_leave: bool,
    holiday: bool,
    weekly_off: bool,
) -> AttendanceDayStatus {
    if holiday {
        return AttendanceDayStatus::Holiday;
    }
    if on_leave {
        return AttendanceDayStatus::OnLeave;
    }
    if weekly_off {
        return AttendanceDayStatus::WeeklyOff;
    }
    let Some(expected_minutes) = expected_minutes.filter(|value| *value > 0) else {
        return AttendanceDayStatus::Unscheduled;
    };
    if has_open_segment {
        return AttendanceDayStatus::Incomplete;
    }
    if logged_minutes <= 0 {
        return AttendanceDayStatus::Absent;
    }
    if logged_minutes >= expected_minutes {
        return AttendanceDayStatus::Present;
    }
    if logged_minutes >= (expected_minutes + 1) / 2 {
        return AttendanceDayStatus::HalfDay;
    }
    AttendanceDayStatus::Incomplete
}

#[derive(Clone, Debug)]
pub struct AttendanceDailyReportRow {
    pub employee_id: Uuid,
    pub employee_name: String,
    pub employee_code: String,
    pub work_date: NaiveDate,
    pub timezone: String,
    pub first_check_in_at: Option<DateTime<Utc>>,
    pub last_check_out_at: Option<DateTime<Utc>>,
    pub logged_minutes: i32,
    pub expected_minutes: Option<i32>,
    pub status: AttendanceDayStatus,
    pub segment_count: i32,
}

#[derive(Clone, Debug, Default)]
pub struct AttendanceReportSummary {
    pub total_days: i32,
    pub present_days: i32,
    pub half_days: i32,
    pub absent_days: i32,
    pub on_leave_days: i32,
    pub holiday_days: i32,
    pub weekly_off_days: i32,
    pub incomplete_days: i32,
    pub unscheduled_days: i32,
    pub total_logged_minutes: i64,
}

#[derive(Clone, Debug)]
pub struct AttendanceReportPage {
    pub rows: Vec<AttendanceDailyReportRow>,
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

#[derive(Clone, Debug)]
pub struct AttendanceReportData {
    pub page: AttendanceReportPage,
    pub summary: AttendanceReportSummary,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
struct ReportCursor {
    work_date: NaiveDate,
    employee_id: Uuid,
}

pub fn cursor_for_row(row: &AttendanceDailyReportRow) -> String {
    ReportCursor {
        work_date: row.work_date,
        employee_id: row.employee_id,
    }
    .encode()
}

impl ReportCursor {
    fn encode(self) -> String {
        URL_SAFE_NO_PAD.encode(format!("{}|{}", self.work_date, self.employee_id))
    }

    fn decode(raw: &str) -> KabiPayResult<Self> {
        let decoded = URL_SAFE_NO_PAD
            .decode(raw.trim())
            .map_err(|_| KabiPayError::Validation("invalid attendance report cursor".into()))?;
        let text = std::str::from_utf8(&decoded)
            .map_err(|_| KabiPayError::Validation("invalid attendance report cursor".into()))?;
        let (date, employee_id) = text
            .split_once('|')
            .ok_or_else(|| KabiPayError::Validation("invalid attendance report cursor".into()))?;
        Ok(Self {
            work_date: date
                .parse()
                .map_err(|_| KabiPayError::Validation("invalid attendance report cursor".into()))?,
            employee_id: employee_id
                .parse()
                .map_err(|_| KabiPayError::Validation("invalid attendance report cursor".into()))?,
        })
    }
}

fn normalize_search(value: &str) -> String {
    value.split_whitespace().collect::<Vec<_>>().join(" ").to_lowercase()
}

fn shift_expected_minutes(row: &shift::Model) -> Option<i32> {
    if let Some(hours) = row.work_hours.filter(|hours| *hours > 0) {
        return hours.checked_mul(60);
    }
    let (Some(start), Some(end)) = (row.start_time, row.end_time) else {
        return None;
    };
    let start = i64::from(start.num_seconds_from_midnight());
    let mut end = i64::from(end.num_seconds_from_midnight());
    if row.is_night_shift && end <= start {
        end += 24 * 60 * 60;
    }
    let seconds = end - start;
    (seconds > 0).then_some((seconds / 60) as i32)
}

fn is_weekly_off(date: NaiveDate, expected_minutes: Option<i32>) -> bool {
    expected_minutes.is_none() && matches!(date.weekday(), Weekday::Sat | Weekday::Sun)
}

fn canonical_instants(
    row: &attendance::Model,
    clock: TenantBusinessClock,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let legacy_check_in = row
        .check_in_time
        .and_then(|time| clock.to_utc(row.work_date, time).ok());
    let legacy_check_out = match (row.check_in_time, row.check_out_time) {
        (Some(check_in), Some(check_out)) if check_out != check_in => {
            let checkout_date = if check_out > check_in {
                Some(row.work_date)
            } else {
                row.work_date.checked_add_days(Days::new(1))
            };
            checkout_date.and_then(|date| clock.to_utc(date, check_out).ok())
        }
        _ => None,
    };
    let check_in = row.check_in_at.or(legacy_check_in);
    let check_out = row.check_out_at.or(legacy_check_out);
    (check_in, check_out)
}

fn aggregate_segments(
    rows: &[attendance::Model],
    clock: TenantBusinessClock,
) -> (i32, i32, bool, Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let mut total_seconds = 0i64;
    let mut has_open = false;
    let mut first_check_in = None;
    let mut last_check_out = None;
    for row in rows {
        let (check_in, check_out) = canonical_instants(row, clock);
        if check_in.is_some() && check_out.is_none() {
            has_open = true;
        }
        if let Some(value) = check_in {
            first_check_in = Some(first_check_in.map_or(value, |current: DateTime<Utc>| current.min(value)));
        }
        if let Some(value) = check_out {
            last_check_out = Some(last_check_out.map_or(value, |current: DateTime<Utc>| current.max(value)));
        }
        if let (Some(start), Some(end)) = (check_in, check_out) {
            total_seconds += end.signed_duration_since(start).num_seconds().max(0);
        }
    }
    let rounded_minutes = ((total_seconds as f64) / 60.0).round() as i32;
    (rounded_minutes, rows.len() as i32, has_open, first_check_in, last_check_out)
}

fn summarize(rows: &[AttendanceDailyReportRow]) -> AttendanceReportSummary {
    let mut summary = AttendanceReportSummary::default();
    for row in rows {
        summary.total_days += 1;
        summary.total_logged_minutes += i64::from(row.logged_minutes);
        match row.status {
            AttendanceDayStatus::Present => summary.present_days += 1,
            AttendanceDayStatus::HalfDay => summary.half_days += 1,
            AttendanceDayStatus::Absent => summary.absent_days += 1,
            AttendanceDayStatus::OnLeave => summary.on_leave_days += 1,
            AttendanceDayStatus::Holiday => summary.holiday_days += 1,
            AttendanceDayStatus::WeeklyOff => summary.weekly_off_days += 1,
            AttendanceDayStatus::Incomplete => summary.incomplete_days += 1,
            AttendanceDayStatus::Unscheduled => summary.unscheduled_days += 1,
        }
    }
    summary
}

#[allow(clippy::too_many_arguments)]
pub async fn attendance_report(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    scope: &EmployeeScopeFilter,
    clock: TenantBusinessClock,
    from_date: NaiveDate,
    to_date: NaiveDate,
    employee_id: Option<Uuid>,
    employee_search: Option<&str>,
    first: Option<i32>,
    after: Option<&str>,
) -> KabiPayResult<AttendanceReportData> {
    if to_date < from_date || to_date.signed_duration_since(from_date).num_days() >= MAX_REPORT_DAYS {
        return Err(KabiPayError::Validation(
            "attendance report date range must be between 1 and 92 calendar days".into(),
        ));
    }
    let page_size = first.unwrap_or(DEFAULT_PAGE_SIZE as i32);
    if !(1..=MAX_PAGE_SIZE as i32).contains(&page_size) {
        return Err(KabiPayError::Validation(
            "first must be between 1 and 100".into(),
        ));
    }
    let after = after.map(ReportCursor::decode).transpose()?;

    let mut employee_query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::DateOfJoining.lte(to_date));
    match scope {
        EmployeeScopeFilter::Empty => {
            return Ok(AttendanceReportData {
                page: AttendanceReportPage {
                    rows: vec![],
                    end_cursor: None,
                    has_next_page: false,
                },
                summary: AttendanceReportSummary::default(),
            });
        }
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => {
            return Ok(AttendanceReportData {
                page: AttendanceReportPage {
                    rows: vec![],
                    end_cursor: None,
                    has_next_page: false,
                },
                summary: AttendanceReportSummary::default(),
            });
        }
        EmployeeScopeFilter::EmployeeIds(ids) => {
            employee_query = employee_query.filter(employee::Column::Id.is_in(ids.clone()));
        }
        EmployeeScopeFilter::Unrestricted => {}
    }
    if let Some(employee_id) = employee_id {
        employee_query = employee_query.filter(employee::Column::Id.eq(employee_id));
    }
    let mut employees = employee_query
        .order_by_asc(employee::Column::EmployeeCode)
        .all(db)
        .await?;
    if let Some(search) = employee_search.map(normalize_search).filter(|value| !value.is_empty()) {
        employees.retain(|row| {
            normalize_search(&format!("{} {}", row.first_name, row.last_name)).contains(&search)
                || normalize_search(&row.employee_code).contains(&search)
        });
    }
    let employee_ids: Vec<Uuid> = employees.iter().map(|row| row.id).collect();
    if employee_ids.is_empty() {
        return Ok(AttendanceReportData {
            page: AttendanceReportPage { rows: vec![], end_cursor: None, has_next_page: false },
            summary: AttendanceReportSummary::default(),
        });
    }

    let attendance_rows = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.is_in(employee_ids.clone()))
        .filter(attendance::Column::WorkDate.gte(from_date))
        .filter(attendance::Column::WorkDate.lte(to_date))
        .all(db)
        .await?;
    let mut attendance_by_day: HashMap<(Uuid, NaiveDate), Vec<attendance::Model>> = HashMap::new();
    for row in attendance_rows {
        attendance_by_day.entry((row.employee_id, row.work_date)).or_default().push(row);
    }

    let shifts: HashMap<Uuid, shift::Model> = shift::Entity::find()
        .filter(shift::Column::TenantId.eq(tenant_id))
        .all(db)
        .await?
        .into_iter()
        .map(|row| (row.id, row))
        .collect();
    let employee_shifts = employee_shift::Entity::find()
        .filter(employee_shift::Column::TenantId.eq(tenant_id))
        .filter(employee_shift::Column::EmployeeId.is_in(employee_ids.clone()))
        .filter(employee_shift::Column::EffectiveFrom.lte(to_date))
        .order_by_desc(employee_shift::Column::EffectiveFrom)
        .all(db)
        .await?;
    let roster_slots: HashMap<(Uuid, NaiveDate), Uuid> = roster_slot::Entity::find()
        .filter(roster_slot::Column::TenantId.eq(tenant_id))
        .filter(roster_slot::Column::EmployeeId.is_in(employee_ids.clone()))
        .filter(roster_slot::Column::SlotDate.gte(from_date))
        .filter(roster_slot::Column::SlotDate.lte(to_date))
        .all(db)
        .await?
        .into_iter()
        .map(|row| ((row.employee_id, row.slot_date), row.shift_id))
        .collect();

    let calendars = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .all(db)
        .await?;
    let calendar_locations: HashMap<Uuid, Option<Uuid>> =
        calendars.iter().map(|row| (row.id, row.location_id)).collect();
    let holidays = if calendar_locations.is_empty() {
        vec![]
    } else {
        holiday::Entity::find()
            .filter(holiday::Column::CalendarId.is_in(calendar_locations.keys().copied()))
            .filter(holiday::Column::HolidayDate.gte(from_date))
            .filter(holiday::Column::HolidayDate.lte(to_date))
            .all(db)
            .await?
    };
    let holiday_locations: HashSet<(NaiveDate, Option<Uuid>)> = holidays
        .into_iter()
        .filter_map(|row| calendar_locations.get(&row.calendar_id).map(|location| (row.holiday_date, *location)))
        .collect();

    let leaves = leave_request::Entity::find()
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::EmployeeId.is_in(employee_ids.clone()))
        .filter(leave_request::Column::Status.eq("APPROVED"))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .filter(leave_request::Column::FromDate.lte(to_date))
        .filter(leave_request::Column::ToDate.gte(from_date))
        .all(db)
        .await?;
    let separation_dates: HashMap<Uuid, NaiveDate> = separation::Entity::find()
        .filter(separation::Column::TenantId.eq(tenant_id))
        .filter(separation::Column::EmployeeId.is_in(employee_ids))
        .filter(separation::Column::Status.eq("APPROVED"))
        .all(db)
        .await?
        .into_iter()
        .fold(HashMap::new(), |mut dates, row| {
            dates
                .entry(row.employee_id)
                .and_modify(|current| *current = (*current).min(row.last_working_date))
                .or_insert(row.last_working_date);
            dates
        });

    let mut rows = Vec::new();
    for employee in employees {
        let mut date = from_date.max(employee.date_of_joining);
        let employment_end = separation_dates
            .get(&employee.id)
            .copied()
            .unwrap_or(to_date)
            .min(to_date);
        while date <= employment_end {
            let segments = attendance_by_day.get(&(employee.id, date)).map(Vec::as_slice).unwrap_or(&[]);
            let (logged_minutes, segment_count, has_open, first_check_in_at, last_check_out_at) =
                aggregate_segments(segments, clock);
            let scheduled_shift_id = roster_slots.get(&(employee.id, date)).copied().or_else(|| {
                employee_shifts.iter().find(|assignment| {
                    assignment.employee_id == employee.id
                        && assignment.effective_from <= date
                        && assignment.effective_to.is_none_or(|end| end >= date)
                }).map(|assignment| assignment.shift_id)
            });
            let expected_minutes = scheduled_shift_id
                .and_then(|shift_id| shifts.get(&shift_id))
                .and_then(shift_expected_minutes);
            let is_holiday = holiday_locations.contains(&(date, None))
                || employee.location_id.is_some_and(|location| holiday_locations.contains(&(date, Some(location))));
            let is_on_leave = leaves.iter().any(|leave| {
                leave.employee_id == employee.id && leave.from_date <= date && leave.to_date >= date
            });
            let weekly_off = is_weekly_off(date, expected_minutes);
            rows.push(AttendanceDailyReportRow {
                employee_id: employee.id,
                employee_name: format!("{} {}", employee.first_name, employee.last_name).trim().to_owned(),
                employee_code: employee.employee_code.clone(),
                work_date: date,
                timezone: clock.timezone_name().to_owned(),
                first_check_in_at,
                last_check_out_at,
                logged_minutes,
                expected_minutes,
                status: classify_day(expected_minutes, logged_minutes, has_open, is_on_leave, is_holiday, weekly_off),
                segment_count,
            });
            date = date.checked_add_signed(Duration::days(1)).ok_or_else(|| {
                KabiPayError::Validation("attendance report date range overflow".into())
            })?;
        }
    }
    rows.sort_by(|left, right| {
        right.work_date.cmp(&left.work_date).then_with(|| left.employee_id.cmp(&right.employee_id))
    });
    let summary = summarize(&rows);
    if let Some(cursor) = after {
        rows.retain(|row| (row.work_date, row.employee_id) < (cursor.work_date, cursor.employee_id));
    }
    let has_next_page = rows.len() > page_size as usize;
    rows.truncate(page_size as usize);
    let end_cursor = rows.last().map(|row| ReportCursor {
        work_date: row.work_date,
        employee_id: row.employee_id,
    }.encode());
    Ok(AttendanceReportData {
        page: AttendanceReportPage { rows, end_cursor, has_next_page },
        summary,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_thresholds_are_derived_from_expected_minutes() {
        assert_eq!(classify_day(Some(480), 480, false, false, false, false), AttendanceDayStatus::Present);
        assert_eq!(classify_day(Some(420), 210, false, false, false, false), AttendanceDayStatus::HalfDay);
        assert_eq!(classify_day(Some(420), 0, false, false, false, false), AttendanceDayStatus::Absent);
        assert_eq!(classify_day(None, 0, false, false, false, false), AttendanceDayStatus::Unscheduled);
    }

    #[test]
    fn calendar_and_incomplete_states_override_duration_thresholds() {
        assert_eq!(classify_day(Some(480), 480, false, true, false, false), AttendanceDayStatus::OnLeave);
        assert_eq!(classify_day(Some(480), 480, false, false, true, false), AttendanceDayStatus::Holiday);
        assert_eq!(classify_day(Some(480), 480, false, false, false, true), AttendanceDayStatus::WeeklyOff);
        assert_eq!(classify_day(Some(480), 120, true, false, false, false), AttendanceDayStatus::Incomplete);
    }

    #[test]
    fn a_scheduled_weekend_is_classified_from_shift_duration() {
        let saturday: NaiveDate = "2026-08-29".parse().unwrap();
        assert!(!is_weekly_off(saturday, Some(480)));
        assert!(is_weekly_off(saturday, None));
    }
}
