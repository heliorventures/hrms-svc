//! GraphQL DTOs for kabipay-attendance.

use async_graphql::{ComplexObject, Context, InputObject, Result, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use kabipay_common::client_data_scope::{data_scope_from_context, resolve_viewer_employee};
use kabipay_common::context::PERM_TIMESHEET_APPROVE;
use kabipay_common::subgraph::{require_client_claims, require_tenant_id, tenant_db};
use kabipay_common::workflow_approval::WorkflowApprovalAuthority;
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
use crate::services::attendance_management_service::ManagedAttendanceRow;
use crate::services::attendance_report_service::{
    AttendanceDailyReportRow, AttendanceReportSummary,
};

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

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceEdge")]
pub struct AttendanceEdgeDto {
    pub cursor: String,
    pub node: AttendanceDto,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ManagedAttendance")]
pub struct ManagedAttendanceDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub shift_id: Option<ID>,
    pub work_date: NaiveDate,
    pub check_in_time: Option<NaiveTime>,
    pub check_out_time: Option<NaiveTime>,
    pub check_in_lat: Option<String>,
    pub check_in_lng: Option<String>,
    pub check_out_lat: Option<String>,
    pub check_out_lng: Option<String>,
    pub status: Option<String>,
    pub source: Option<String>,
    pub late_minutes: Option<i32>,
    pub employee_name: String,
    pub employee_code: String,
    pub regularization_status: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ManagedAttendanceEdge")]
pub struct ManagedAttendanceEdgeDto {
    pub cursor: String,
    pub node: ManagedAttendanceDto,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendancePageInfo")]
pub struct AttendancePageInfoDto {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceConnection")]
pub struct AttendanceConnectionDto {
    pub edges: Vec<AttendanceEdgeDto>,
    pub page_info: AttendancePageInfoDto,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ManagedAttendanceConnection")]
pub struct ManagedAttendanceConnectionDto {
    pub edges: Vec<ManagedAttendanceEdgeDto>,
    pub page_info: AttendancePageInfoDto,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceDailyReportRow")]
pub struct AttendanceDailyReportRowDto {
    pub employee_id: ID,
    pub employee_name: String,
    pub employee_code: String,
    pub work_date: NaiveDate,
    pub timezone: String,
    pub first_check_in_at: Option<DateTime<Utc>>,
    pub last_check_out_at: Option<DateTime<Utc>>,
    pub logged_minutes: i32,
    pub expected_minutes: Option<i32>,
    pub status: String,
    pub segment_count: i32,
}

impl From<AttendanceDailyReportRow> for AttendanceDailyReportRowDto {
    fn from(row: AttendanceDailyReportRow) -> Self {
        Self {
            employee_id: ID(row.employee_id.to_string()),
            employee_name: row.employee_name,
            employee_code: row.employee_code,
            work_date: row.work_date,
            timezone: row.timezone,
            first_check_in_at: row.first_check_in_at,
            last_check_out_at: row.last_check_out_at,
            logged_minutes: row.logged_minutes,
            expected_minutes: row.expected_minutes,
            status: row.status.as_str().into(),
            segment_count: row.segment_count,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceDailyReportEdge")]
pub struct AttendanceDailyReportEdgeDto {
    pub cursor: String,
    pub node: AttendanceDailyReportRowDto,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceDailyReportConnection")]
pub struct AttendanceDailyReportConnectionDto {
    pub edges: Vec<AttendanceDailyReportEdgeDto>,
    pub page_info: AttendancePageInfoDto,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AttendanceReportSummary")]
pub struct AttendanceReportSummaryDto {
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

impl From<AttendanceReportSummary> for AttendanceReportSummaryDto {
    fn from(summary: AttendanceReportSummary) -> Self {
        Self {
            total_days: summary.total_days,
            present_days: summary.present_days,
            half_days: summary.half_days,
            absent_days: summary.absent_days,
            on_leave_days: summary.on_leave_days,
            holiday_days: summary.holiday_days,
            weekly_off_days: summary.weekly_off_days,
            incomplete_days: summary.incomplete_days,
            unscheduled_days: summary.unscheduled_days,
            total_logged_minutes: summary.total_logged_minutes,
        }
    }
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

#[derive(InputObject, Clone, Debug)]
pub struct AddManagedAttendanceSegmentInput {
    pub employee_id: ID,
    pub work_date: NaiveDate,
    pub check_in_time: NaiveTime,
    pub check_out_time: NaiveTime,
    pub reason: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpdateManagedAttendanceSegmentInput {
    pub id: ID,
    pub work_date: NaiveDate,
    pub check_in_time: NaiveTime,
    pub check_out_time: NaiveTime,
    pub reason: String,
    pub expected_updated_at: DateTime<Utc>,
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
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope: data_scope_from_context(ctx, PERM_TIMESHEET_APPROVE)?,
            permission: PERM_TIMESHEET_APPROVE,
        };
        let employee_id = parse_uuid(&self.employee_id, "employeeId")?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        timesheet_batch_service::timesheet_week_batch_viewer_may_approve(
            &db,
            tenant_id,
            &self.status,
            employee_id,
            wf,
            &authority,
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

impl From<ManagedAttendanceRow> for ManagedAttendanceDto {
    fn from(row: ManagedAttendanceRow) -> Self {
        let attendance = row.attendance;
        Self {
            id: ID(attendance.id.to_string()),
            tenant_id: ID(attendance.tenant_id.to_string()),
            employee_id: ID(attendance.employee_id.to_string()),
            shift_id: attendance.shift_id.map(|id| ID(id.to_string())),
            work_date: attendance.work_date,
            check_in_time: attendance.check_in_time,
            check_out_time: attendance.check_out_time,
            check_in_lat: attendance.check_in_lat.map(|value| value.to_string()),
            check_in_lng: attendance.check_in_lng.map(|value| value.to_string()),
            check_out_lat: attendance.check_out_lat.map(|value| value.to_string()),
            check_out_lng: attendance.check_out_lng.map(|value| value.to_string()),
            status: attendance.status,
            source: attendance.source,
            late_minutes: attendance.late_minutes,
            employee_name: row.employee_name,
            employee_code: row.employee_code,
            regularization_status: attendance.regularization_status,
            created_at: attendance.created_at,
            updated_at: attendance.updated_at,
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
