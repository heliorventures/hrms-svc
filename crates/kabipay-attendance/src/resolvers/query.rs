//! Root query resolvers for kabipay-attendance.

use async_graphql::{Context, Object, Result, ID};
use chrono::{Duration, NaiveDate};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_context, resolve_employee_scope_filter, resolve_viewer_employee,
    },
    context::{PERM_ATTENDANCE_READ, PERM_TIMESHEET_APPROVE, PERM_TIMESHEET_READ},
    subgraph::{ops_db, require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    tenant_business_clock::TenantBusinessClock,
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AttendanceAdjustmentPolicyDto, AttendanceConnectionDto, AttendanceDto, AttendanceEdgeDto,
    AttendanceDailyReportConnectionDto, AttendanceDailyReportEdgeDto, AttendancePageInfoDto,
    AttendancePunchPolicyDto, AttendanceReportSummaryDto, HolidayCalendarDto, HolidayDayDto,
    HolidayEntryDto, ManagedAttendanceConnectionDto, ManagedAttendanceDto,
    ManagedAttendanceEdgeDto, PunchDaySummaryDto, ShiftDto, TimesheetEntryDto,
    TimesheetLockPolicyDto, TimesheetProjectOptionDto, TimesheetWeekBatchDto,
};
use crate::resolvers::attendance_management_auth;
use crate::resolvers::timesheet_assignment_auth;
use crate::services::{
    attendance_management_service, attendance_report_service, attendance_service,
    hrms_master_service, punch_policy, timesheet_batch_service, timesheet_project_assignment_service,
};

fn self_attendance_date_range(
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
    today: NaiveDate,
) -> Result<(NaiveDate, NaiveDate)> {
    let to_date = to_date.unwrap_or(today);
    let from_date = match from_date {
        Some(value) => value,
        None => to_date.checked_sub_signed(Duration::days(91)).ok_or_else(|| {
            KabiPayError::Validation("invalid attendance date range".into()).into_graphql()
        })?,
    };
    attendance_management_service::validate_date_range(from_date, to_date)
        .map_err(KabiPayError::into_graphql)?;
    Ok((from_date, to_date))
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn attendance_health(&self) -> &'static str {
        "ok"
    }

    /// Live punch policy (geofence + IP). Requires `attendance:punch_policy`.
    async fn attendance_punch_policy(&self, ctx: &Context<'_>) -> Result<AttendancePunchPolicyDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_configure_attendance_punch_policy() {
            return Err(KabiPayError::Forbidden(
                "attendance punch policy permission is required".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let row = punch_policy::find_punch_policy(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(match row {
            Some(m) => AttendancePunchPolicyDto::from(m),
            None => AttendancePunchPolicyDto::not_configured(tenant_id),
        })
    }

    /// List all shift templates for the caller's tenant.
    async fn shifts(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<ShiftDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = attendance_service::list_shifts(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ShiftDto::from).collect())
    }

    /// Recent attendance rows for the caller's tenant, newest first.
    async fn attendance(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
        from_date: Option<NaiveDate>,
        to_date: Option<NaiveDate>,
    ) -> Result<Vec<AttendanceDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, PERM_ATTENDANCE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows = attendance_service::list_attendance(&db, tenant_id, limit, &filt, from_date, to_date)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AttendanceDto::from).collect())
    }

    /// Cursor-paginated attendance for the JWT-linked employee only.
    async fn my_attendance(
        &self,
        ctx: &Context<'_>,
        from_date: Option<NaiveDate>,
        to_date: Option<NaiveDate>,
        first: Option<i32>,
        after: Option<String>,
    ) -> Result<AttendanceConnectionDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let (from_date, to_date) =
            self_attendance_date_range(from_date, to_date, clock.now_date())?;
        let page = attendance_management_service::list_my_attendance(
            &db,
            tenant_id,
            employee_id,
            from_date,
            to_date,
            first,
            after.as_deref(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceConnectionDto {
            edges: page
                .rows
                .into_iter()
                .map(|row| AttendanceEdgeDto {
                    cursor: attendance_management_service::AttendanceCursor::new(
                        row.work_date,
                        row.created_at,
                        row.id,
                    )
                    .encode(),
                    node: row.into(),
                })
                .collect(),
            page_info: AttendancePageInfoDto {
                end_cursor: page.end_cursor,
                has_next_page: page.has_next_page,
            },
        })
    }

    /// Cursor-paginated attendance for active employees within the caller's explicit attendance scope.
    async fn managed_attendance(
        &self,
        ctx: &Context<'_>,
        from_date: NaiveDate,
        to_date: NaiveDate,
        employee_search: Option<String>,
        employee_id: Option<ID>,
        first: Option<i32>,
        after: Option<String>,
    ) -> Result<ManagedAttendanceConnectionDto> {
        let tenant_id = require_tenant_id(ctx)?;
        attendance_management_auth::require_regularizer(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = attendance_management_auth::scope_filter(ctx, &db, tenant_id).await?;
        let employee_id = employee_id
            .as_ref()
            .map(|value| parse_uuid(value, "employeeId"))
            .transpose()?;
        if let Some(employee_id) = employee_id {
            attendance_management_auth::assert_target_in_resolved_scope(
                &db,
                tenant_id,
                &scope,
                employee_id,
            )
            .await?;
        }
        let page = attendance_management_service::list_managed_attendance(
            &db,
            tenant_id,
            &scope,
            from_date,
            to_date,
            employee_search.as_deref(),
            employee_id,
            first,
            after.as_deref(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ManagedAttendanceConnectionDto {
            edges: page
                .rows
                .into_iter()
                .map(|row| {
                    let cursor = attendance_management_service::AttendanceCursor::new(
                        row.attendance.work_date,
                        row.attendance.created_at,
                        row.attendance.id,
                    )
                    .encode();
                    ManagedAttendanceEdgeDto {
                        cursor,
                        node: ManagedAttendanceDto::from(row),
                    }
                })
                .collect(),
            page_info: AttendancePageInfoDto {
                end_cursor: page.end_cursor,
                has_next_page: page.has_next_page,
            },
        })
    }

    /// Policy-derived, cursor-paginated daily attendance report for the caller's
    /// exact `attendance:read` scope.
    async fn attendance_daily_report(
        &self,
        ctx: &Context<'_>,
        from_date: NaiveDate,
        to_date: NaiveDate,
        employee_id: Option<ID>,
        employee_search: Option<String>,
        first: Option<i32>,
        after: Option<String>,
    ) -> Result<AttendanceDailyReportConnectionDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, PERM_ATTENDANCE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filter = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let employee_id = employee_id
            .as_ref()
            .map(|value| parse_uuid(value, "employeeId"))
            .transpose()?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let report = attendance_report_service::attendance_report(
            &db,
            tenant_id,
            &filter,
            clock,
            from_date,
            to_date,
            employee_id,
            employee_search.as_deref(),
            first,
            after.as_deref(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceDailyReportConnectionDto {
            edges: report
                .page
                .rows
                .into_iter()
                .map(|row| AttendanceDailyReportEdgeDto {
                    cursor: attendance_report_service::cursor_for_row(&row),
                    node: row.into(),
                })
                .collect(),
            page_info: AttendancePageInfoDto {
                end_cursor: report.page.end_cursor,
                has_next_page: report.page.has_next_page,
            },
        })
    }

    /// Complete summary over the same authorized and filtered projection used
    /// by `attendanceDailyReport`; it is never derived from a browser page.
    async fn attendance_report_summary(
        &self,
        ctx: &Context<'_>,
        from_date: NaiveDate,
        to_date: NaiveDate,
        employee_id: Option<ID>,
        employee_search: Option<String>,
    ) -> Result<AttendanceReportSummaryDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, PERM_ATTENDANCE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filter = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let employee_id = employee_id
            .as_ref()
            .map(|value| parse_uuid(value, "employeeId"))
            .transpose()?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let report = attendance_report_service::attendance_report(
            &db,
            tenant_id,
            &filter,
            clock,
            from_date,
            to_date,
            employee_id,
            employee_search.as_deref(),
            Some(1),
            None,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(report.summary.into())
    }

    /// Multi-segment punch for one work day: total worked minutes and all segments
    /// (JWT employee; `workDate` defaults to today in the tenant business timezone).
    async fn punch_day_summary(
        &self,
        ctx: &Context<'_>,
        work_date: Option<NaiveDate>,
    ) -> Result<PunchDaySummaryDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_record_own_attendance_punches() {
            return Err(
                KabiPayError::Forbidden(
                    "attendance:punch_self permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let date = work_date.unwrap_or_else(|| clock.now_date());
        let s = attendance_service::punch_day_summary(&db, tenant_id, employee_id, date)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(s.into())
    }

    /// Holidays on or after `fromDate` (defaults to today), all calendars in the tenant.
    async fn upcoming_holidays(
        &self,
        ctx: &Context<'_>,
        from_date: Option<NaiveDate>,
        #[graphql(default = 30)] limit: u64,
    ) -> Result<Vec<HolidayEntryDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let from = from_date.unwrap_or_else(|| clock.now_date());
        let rows = attendance_service::list_upcoming_holidays(&db, tenant_id, from, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|(h, n)| HolidayEntryDto::from_holiday(h, n))
            .collect())
    }

    /// Timesheet rows for an employee. Omit `employeeId` to use the JWT-linked employee.
    async fn timesheet_entries(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
        from_date: Option<NaiveDate>,
        to_date: Option<NaiveDate>,
    ) -> Result<Vec<TimesheetEntryDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = if let Some(id) = &employee_id {
            parse_uuid(id, "employeeId")?
        } else {
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        let scope = data_scope_from_context(ctx, PERM_TIMESHEET_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if !filt.allows_employee(emp) {
            return Ok(vec![]);
        }
        let rows = attendance_service::list_timesheet_entries(
            &db,
            tenant_id,
            emp,
            limit,
            from_date,
            to_date,
        )
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TimesheetEntryDto::from).collect())
    }

    async fn attendance_adjustment_policy(&self, ctx: &Context<'_>) -> Result<AttendanceAdjustmentPolicyDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let p = hrms_master_service::load_attendance_adjustment_policy(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceAdjustmentPolicyDto {
            max_self_adjust_days: p.max_self_adjust_days,
        })
    }

    async fn timesheet_lock_policy(&self, ctx: &Context<'_>) -> Result<TimesheetLockPolicyDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let p = hrms_master_service::load_timesheet_lock_policy(&db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(TimesheetLockPolicyDto {
            editable_week_span: p.editable_week_span,
            lock_approved_entries: p.lock_approved_entries,
        })
    }

    async fn timesheet_projects(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TimesheetProjectOptionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = hrms_master_service::list_projects(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|m| TimesheetProjectOptionDto {
                code: m.data_key,
                name: m.value,
            })
            .collect())
    }

    async fn timesheet_task_types(
        &self,
        ctx: &Context<'_>,
        project_code: String,
    ) -> Result<Vec<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = hrms_master_service::list_task_rows_for_project(&db, tenant_id, project_code.trim())
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some(first) = rows.into_iter().next() else {
            return Ok(vec![]);
        };
        serde_json::from_str::<Vec<String>>(&first.value).map_err(|e| {
            KabiPayError::Validation(format!("task types JSON: {e}")).into_graphql()
        })
    }

    /// Projects the employee may log hours against (full catalog when no per-employee assignments exist).
    /// Omit `employeeId` for the JWT-linked employee.
    async fn timesheet_projects_for_employee(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TimesheetProjectOptionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let target = if let Some(id) = &employee_id {
            parse_uuid(id, "employeeId")?
        } else {
            resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?
        };
        timesheet_assignment_auth::assert_can_read_employee_assignment_target(
            ctx, &db, tenant_id, target,
        )
        .await?;
        let rows = timesheet_project_assignment_service::visible_projects_for_employee(
            &db, tenant_id, target, limit,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|m| TimesheetProjectOptionDto {
                code: m.data_key,
                name: m.value,
            })
            .collect())
    }

    /// Assigned project codes only (empty ⇒ unrestricted catalog).
    async fn employee_timesheet_project_codes(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
    ) -> Result<Vec<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let target = parse_uuid(&employee_id, "employeeId")?;
        timesheet_assignment_auth::assert_can_read_employee_assignment_target(
            ctx, &db, tenant_id, target,
        )
        .await?;
        timesheet_project_assignment_service::list_assigned_codes(&db, tenant_id, target)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn timesheet_week_batches(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        #[graphql(default = 80)] limit: u64,
    ) -> Result<Vec<TimesheetWeekBatchDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_approve_timesheet_requests() {
            return Err(KabiPayError::Forbidden(
                "timesheet week queue requires timesheet approval permission".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        // Use `timesheet` resource so JWT `resource_scopes` matches `permission_scope` (e.g. LINE_MANAGER TEAM).
        // `attendance` defaults to Self for managers and would hide direct reports' batches.
        let scope = data_scope_from_context(ctx, PERM_TIMESHEET_APPROVE)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows =
            timesheet_batch_service::list_timesheet_week_batches(&db, tenant_id, status, limit, &filt)
                .await
                .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TimesheetWeekBatchDto::from).collect())
    }

    /// Admin: list holiday calendars (tenant). Requires leave configuration permission.
    async fn holiday_calendars(
        &self,
        ctx: &Context<'_>,
        year: Option<i32>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<HolidayCalendarDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_leave_configuration() {
            return Err(
                KabiPayError::Forbidden("holiday calendar admin requires leave configuration permission".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = attendance_service::list_holiday_calendars(&db, tenant_id, year, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(HolidayCalendarDto::from).collect())
    }

    /// Admin: holidays in a calendar. Requires leave configuration permission.
    async fn holidays_in_calendar(
        &self,
        ctx: &Context<'_>,
        calendar_id: ID,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<HolidayDayDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_leave_configuration() {
            return Err(
                KabiPayError::Forbidden("holiday admin requires leave configuration permission".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&calendar_id, "calendarId")?;
        let rows =
            attendance_service::list_holidays_in_calendar(&db, tenant_id, cid, limit)
                .await
                .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(HolidayDayDto::from).collect())
    }
}

pub(crate) fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}
