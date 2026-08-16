//! GraphQL DTOs for kabipay-attendance.

use async_graphql::{ComplexObject, Context, InputObject, Result, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use kabipay_common::subgraph::{require_client_claims, require_tenant_id, tenant_db};
use kabipay_common::KabiPayError;
use kabipay_db_entities::tenant::d0010_time_shift_roster::{
    attendance, holiday, holiday_calendar, shift, timesheet_entry, timesheet_week_batch,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0032_attendance_punch_policy::attendance_punch_policy;
use rust_decimal::prelude::ToPrimitive;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::resolvers::query::parse_uuid;
use crate::services::timesheet_batch_service;

use crate::services::attendance_service::PunchDaySummary;

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Shift")]
pub struct ShiftDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub start_time: Option<NaiveTime>,
    pub end_time: Option<NaiveTime>,
    pub work_hours: Option<i32>,
    pub is_night_shift: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<shift::Model> for ShiftDto {
    fn from(m: shift::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            start_time: m.start_time,
            end_time: m.end_time,
            work_hours: m.work_hours,
            is_night_shift: m.is_night_shift,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Attendance")]
pub struct AttendanceDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub shift_id: Option<ID>,
    pub work_date: NaiveDate,
    pub check_in_time: Option<NaiveTime>,
    pub check_out_time: Option<NaiveTime>,
    /// WGS84 latitude for punch-in, when recorded (string decimal, matches DB `NUMERIC`).
    pub check_in_lat: Option<String>,
    pub check_in_lng: Option<String>,
    /// WGS84 coordinates for punch-out, when recorded.
    pub check_out_lat: Option<String>,
    pub check_out_lng: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub late_minutes: Option<i32>,
}

/// Optional client GPS (browser / mobile) for the **current** punch (in or out).
#[derive(InputObject, Clone, Debug)]
pub struct PunchTodayInput {
    pub latitude: Option<f64>,
    pub longitude: Option<f64>,
}

/// Log a **completed** check-in and check-out for a **past or today** `workDate` when both
/// live punches were missed. Same calendar day only: check-in time must be before check-out.
#[derive(InputObject, Clone, Debug)]
pub struct AddManualAttendanceSegmentInput {
    pub work_date: NaiveDate,
    pub check_in_time: NaiveTime,
    pub check_out_time: NaiveTime,
}

/// Update an existing completed attendance segment after client-side review.
#[derive(InputObject, Clone, Debug)]
pub struct UpdateManualAttendanceSegmentInput {
    pub id: ID,
    pub work_date: NaiveDate,
    pub check_in_time: NaiveTime,
    pub check_out_time: NaiveTime,
}

/// One work day: all punch segments + sum of completed segment lengths (minutes).
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PunchDaySummary")]
pub struct PunchDaySummaryDto {
    pub work_date: NaiveDate,
    /// Sum of (check out − check in) for every **completed** segment that day.
    pub total_worked_minutes: i32,
    /// Current in-progress row (punched in, not out), if any.
    pub open_segment: Option<AttendanceDto>,
    /// All segment rows for that day, oldest first.
    pub segments: Vec<AttendanceDto>,
}

/// A holiday in a location calendar, with the parent calendar’s display name.
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "HolidayEntry")]
pub struct HolidayEntryDto {
    pub id: ID,
    pub calendar_id: ID,
    pub calendar_name: String,
    pub holiday_date: NaiveDate,
    pub name: String,
    /// Optional category, e.g. NATIONAL, REGIONAL
    pub holiday_type: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(complex)]
#[graphql(name = "TimesheetWeekBatch")]
pub struct TimesheetWeekBatchDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub week_start_date: NaiveDate,
    pub status: String,
    pub workflow_instance_id: Option<ID>,
    pub submitted_at: Option<DateTime<Utc>>,
    pub rejection_reason: Option<String>,
}

impl From<timesheet_week_batch::Model> for TimesheetWeekBatchDto {
    fn from(m: timesheet_week_batch::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            week_start_date: m.week_start_date,
            status: m.status,
            workflow_instance_id: m.workflow_instance_id.map(|u| ID(u.to_string())),
            submitted_at: m.submitted_at,
            rejection_reason: m.rejection_reason,
        }
    }
}

#[ComplexObject]
impl TimesheetWeekBatchDto {
    async fn pending_approval_stage(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        timesheet_batch_service::resolve_timesheet_pending_approval_stage(
            &db, tenant_id, &self.status, wf,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    async fn viewer_may_approve(&self, ctx: &Context<'_>) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let claims = require_client_claims(ctx)?;
        let employee_id = parse_uuid(&self.employee_id, "employeeId")?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        timesheet_batch_service::timesheet_week_batch_viewer_may_approve(
            &db,
            tenant_id,
            claims.sub,
            &self.status,
            employee_id,
            wf,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    async fn employee_code(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&self.employee_id, "employeeId")?;
        let row = employee::Entity::find_by_id(employee_id)
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(&db)
            .await
            .map_err(|error| KabiPayError::from(error).into_graphql())?
            .map(|employee| employee.employee_code);
        Ok(row)
    }

    async fn employee_name(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = parse_uuid(&self.employee_id, "employeeId")?;
        let row = employee::Entity::find_by_id(employee_id)
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(&db)
            .await
            .map_err(|error| KabiPayError::from(error).into_graphql())?
            .map(|employee| format!("{} {}", employee.first_name, employee.last_name).trim().to_string())
            .filter(|name| !name.is_empty());
        Ok(row)
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TimesheetEntry")]
pub struct TimesheetEntryDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub work_date: NaiveDate,
    pub hours_worked: String,
    pub project_code: Option<String>,
    pub description: Option<String>,
    pub status: String,
    pub batch_id: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<timesheet_entry::Model> for TimesheetEntryDto {
    fn from(m: timesheet_entry::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            work_date: m.work_date,
            hours_worked: m.hours_worked.to_string(),
            project_code: m.project_code,
            description: m.description,
            status: m.status,
            batch_id: m.batch_id.map(|u| ID(u.to_string())),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

/// Tenant policy for live punch: optional geofence around a site and/or IP allowlist.
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendancePunchPolicy")]
pub struct AttendancePunchPolicyDto {
    /// Set after the first successful `upsertAttendancePunchPolicy`.
    pub id: Option<ID>,
    pub tenant_id: ID,
    pub is_enforced: bool,
    pub site_latitude: Option<f64>,
    pub site_longitude: Option<f64>,
    pub max_distance_meters: Option<i32>,
    /// Comma-separated IPs or CIDRs (e.g. `203.0.113.10,192.168.0.0/24`).
    pub ip_allowlist: Option<String>,
    pub updated_at: Option<DateTime<Utc>>,
}

impl AttendancePunchPolicyDto {
    pub fn not_configured(tenant_id: Uuid) -> Self {
        Self {
            id: None,
            tenant_id: ID(tenant_id.to_string()),
            is_enforced: false,
            site_latitude: None,
            site_longitude: None,
            max_distance_meters: None,
            ip_allowlist: None,
            updated_at: None,
        }
    }
}

impl From<attendance_punch_policy::Model> for AttendancePunchPolicyDto {
    fn from(m: attendance_punch_policy::Model) -> Self {
        Self {
            id: Some(ID(m.id.to_string())),
            tenant_id: ID(m.tenant_id.to_string()),
            is_enforced: m.is_enforced,
            site_latitude: m.site_latitude.and_then(|d| d.to_f64()),
            site_longitude: m.site_longitude.and_then(|d| d.to_f64()),
            max_distance_meters: m.max_distance_meters,
            ip_allowlist: m.ip_allowlist,
            updated_at: Some(m.updated_at),
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertAttendancePunchPolicyInput {
    pub is_enforced: bool,
    pub site_latitude: Option<f64>,
    pub site_longitude: Option<f64>,
    pub max_distance_meters: Option<i32>,
    pub ip_allowlist: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct CreateTimesheetEntryInput {
    pub work_date: NaiveDate,
    pub hours_worked: String,
    pub project_code: Option<String>,
    pub description: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateTimesheetEntryInput {
    pub id: ID,
    pub work_date: NaiveDate,
    pub hours_worked: String,
    pub project_code: Option<String>,
    pub description: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceAdjustmentPolicy")]
pub struct AttendanceAdjustmentPolicyDto {
    pub max_self_adjust_days: i64,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TimesheetLockPolicy")]
pub struct TimesheetLockPolicyDto {
    pub editable_week_span: i64,
    pub lock_approved_entries: bool,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TimesheetProjectOption")]
pub struct TimesheetProjectOptionDto {
    pub code: String,
    pub name: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertAttendanceAdjustmentPolicyInput {
    pub max_self_adjust_days: i64,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertTimesheetLockPolicyInput {
    pub editable_week_span: i64,
    pub lock_approved_entries: bool,
}

impl HolidayEntryDto {
    pub fn from_holiday(m: holiday::Model, calendar_name: String) -> Self {
        Self {
            id: ID(m.id.to_string()),
            calendar_id: ID(m.calendar_id.to_string()),
            calendar_name,
            holiday_date: m.holiday_date,
            name: m.name,
            holiday_type: m.r#type,
        }
    }
}

impl From<attendance::Model> for AttendanceDto {
    fn from(m: attendance::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            shift_id: m.shift_id.map(|id| ID(id.to_string())),
            work_date: m.work_date,
            check_in_time: m.check_in_time,
            check_out_time: m.check_out_time,
            check_in_lat: m.check_in_lat.map(|d| d.to_string()),
            check_in_lng: m.check_in_lng.map(|d| d.to_string()),
            check_out_lat: m.check_out_lat.map(|d| d.to_string()),
            check_out_lng: m.check_out_lng.map(|d| d.to_string()),
            status: m.status,
            source: m.source,
            late_minutes: m.late_minutes,
        }
    }
}

impl From<PunchDaySummary> for PunchDaySummaryDto {
    fn from(s: PunchDaySummary) -> Self {
        Self {
            work_date: s.work_date,
            total_worked_minutes: s.total_worked_minutes,
            open_segment: s.open_segment.map(AttendanceDto::from),
            segments: s.segments.into_iter().map(AttendanceDto::from).collect(),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "HolidayCalendar")]
pub struct HolidayCalendarDto {
    pub id: ID,
    pub tenant_id: ID,
    pub location_id: Option<ID>,
    pub name: String,
    pub year: i32,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<holiday_calendar::Model> for HolidayCalendarDto {
    fn from(m: holiday_calendar::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            location_id: m.location_id.map(|u| ID(u.to_string())),
            name: m.name,
            year: m.year,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "HolidayDay")]
pub struct HolidayDayDto {
    pub id: ID,
    pub calendar_id: ID,
    pub holiday_date: NaiveDate,
    pub name: String,
    pub holiday_type: Option<String>,
}

impl From<holiday::Model> for HolidayDayDto {
    fn from(m: holiday::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            calendar_id: ID(m.calendar_id.to_string()),
            holiday_date: m.holiday_date,
            name: m.name,
            holiday_type: m.r#type,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertHolidayCalendarInput {
    pub id: Option<ID>,
    pub name: String,
    pub year: i32,
    pub location_id: Option<ID>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertHolidayDayInput {
    pub calendar_id: ID,
    pub id: Option<ID>,
    pub holiday_date: NaiveDate,
    pub name: String,
    pub holiday_type: Option<String>,
}
