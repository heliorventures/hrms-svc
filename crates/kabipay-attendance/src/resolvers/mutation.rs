//! Write operations for attendance (punch in / out) and timesheet entries.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{
        client_request_hints, require_client_claims, require_tenant_id, resolve_client_employee_id,
        tenant_db,
    },
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AddManualAttendanceSegmentInput, AttendanceAdjustmentPolicyDto, AttendanceDto,
    AttendancePunchPolicyDto, CreateTimesheetEntryInput, HolidayCalendarDto, HolidayDayDto,
    PunchTodayInput, TimesheetEntryDto, TimesheetLockPolicyDto, TimesheetWeekBatchDto,
    UpdateManualAttendanceSegmentInput, UpdateTimesheetEntryInput,
    UpsertAttendanceAdjustmentPolicyInput,
    UpsertAttendancePunchPolicyInput, UpsertHolidayCalendarInput, UpsertHolidayDayInput,
    UpsertTimesheetLockPolicyInput,
};
use crate::resolvers::timesheet_assignment_auth;
use crate::services::{
    attendance_service, hrms_master_service, punch_policy, timesheet_batch_service,
    timesheet_project_assignment_service,
};

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn require_leave_configuration_admin(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_leave_configuration() {
        return Err(
            KabiPayError::Forbidden("missing permission to manage leave configuration".into())
                .into_graphql(),
        );
    }
    Ok(())
}

fn require_hrms_timesheet_settings(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if claims.can_configure_attendance_punch_policy() || claims.can_manage_timesheet_configuration()
    {
        return Ok(());
    }
    Err(
        KabiPayError::Forbidden(
            "missing permission — needs attendance punch policy or timesheet manage".into(),
        )
        .into_graphql(),
    )
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Record a punch: closes the **open** segment (punch in without out) if any, otherwise
    /// starts a **new** segment (new `attendance` row). Multiple in/out pairs per `work_date`
    /// are allowed; there is no “third punch” error.
    ///
    /// When `input` includes **both** `latitude` and `longitude` (WGS84), they are stored on
    /// `attendance` as punch-in coordinates for a new row, or punch-out coordinates when closing
    /// an open segment (`check_out_lat` / `check_out_lng` columns).
    async fn punch_today(
        &self,
        ctx: &Context<'_>,
        input: Option<PunchTodayInput>,
    ) -> Result<AttendanceDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_record_own_attendance_punches() {
            return Err(
                KabiPayError::Forbidden(
                    "attendance:punch_self or employee directory permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let geo = match input {
            None => None,
            Some(i) => attendance_service::parse_punch_geo(i.latitude, i.longitude)
                .map_err(KabiPayError::into_graphql)?,
        };
        let hints = client_request_hints(ctx);
        let client_ip = hints.client_ip.as_deref();
        let m = attendance_service::punch_today(&db, tenant_id, employee_id, geo, client_ip)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceDto::from(m))
    }

    /// Create or update the tenant’s live punch policy (geofence + IP allowlist).
    async fn upsert_attendance_punch_policy(
        &self,
        ctx: &Context<'_>,
        input: UpsertAttendancePunchPolicyInput,
    ) -> Result<AttendancePunchPolicyDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_configure_attendance_punch_policy() {
            return Err(KabiPayError::Forbidden(
                "attendance punch policy is restricted to HR / tenant admins".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let m = punch_policy::upsert_punch_policy(
            &db,
            tenant_id,
            input.is_enforced,
            input.site_latitude,
            input.site_longitude,
            input.max_distance_meters,
            input.ip_allowlist,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendancePunchPolicyDto::from(m))
    }

    /// Add a full **in + out** segment for a `workDate` (no future dates) when the user did not
    /// punch live — does not modify `punch_today` behaviour.
    async fn add_manual_attendance_segment(
        &self,
        ctx: &Context<'_>,
        input: AddManualAttendanceSegmentInput,
    ) -> Result<AttendanceDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_record_own_attendance_punches() {
            return Err(
                KabiPayError::Forbidden(
                    "attendance:punch_self or employee directory permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let privileged = claims.can_regularize_attendance_records();
        let m = attendance_service::add_manual_attendance_segment(
            &db,
            tenant_id,
            employee_id,
            input.work_date,
            input.check_in_time,
            input.check_out_time,
            privileged,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceDto::from(m))
    }

    /// Update an existing manual attendance segment with server-side overlap and daily-cap checks.
    async fn update_manual_attendance_segment(
        &self,
        ctx: &Context<'_>,
        input: UpdateManualAttendanceSegmentInput,
    ) -> Result<AttendanceDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_record_own_attendance_punches() {
            return Err(
                KabiPayError::Forbidden(
                    "attendance:punch_self or employee directory permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let attendance_id = parse_uuid(&input.id, "id")?;
        let privileged = claims.can_regularize_attendance_records();
        let m = attendance_service::update_manual_attendance_segment(
            &db,
            tenant_id,
            attendance_id,
            employee_id,
            input.work_date,
            input.check_in_time,
            input.check_out_time,
            privileged,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceDto::from(m))
    }

    async fn create_timesheet_entry(
        &self,
        ctx: &Context<'_>,
        input: CreateTimesheetEntryInput,
    ) -> Result<TimesheetEntryDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let h = attendance_service::parse_hours(&input.hours_worked)
            .map_err(KabiPayError::into_graphql)?;
        let m = attendance_service::create_timesheet_entry(
            &db,
            tenant_id,
            employee_id,
            input.work_date,
            h,
            input.project_code,
            input.description,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TimesheetEntryDto::from(m))
    }

    /// Soft-deletes a row; it must belong to the caller’s employee.
    async fn delete_timesheet_entry(&self, ctx: &Context<'_>, id: ID) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let eid = parse_uuid(&id, "id")?;
        attendance_service::delete_timesheet_entry(&db, tenant_id, employee_id, eid)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn update_timesheet_entry(
        &self,
        ctx: &Context<'_>,
        input: UpdateTimesheetEntryInput,
    ) -> Result<TimesheetEntryDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let eid = parse_uuid(&input.id, "id")?;
        let h = attendance_service::parse_hours(&input.hours_worked)
            .map_err(KabiPayError::into_graphql)?;
        let m = attendance_service::update_timesheet_entry(
            &db,
            tenant_id,
            employee_id,
            eid,
            input.work_date,
            h,
            input.project_code,
            input.description,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TimesheetEntryDto::from(m))
    }

    async fn submit_timesheet_week(
        &self,
        ctx: &Context<'_>,
        week_start_date: chrono::NaiveDate,
    ) -> Result<TimesheetWeekBatchDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let m =
            timesheet_batch_service::submit_timesheet_week(&db, tenant_id, employee_id, week_start_date)
                .await
                .map_err(KabiPayError::into_graphql)?;
        Ok(TimesheetWeekBatchDto::from(m))
    }

    async fn approve_timesheet_week_batch(&self, ctx: &Context<'_>, id: ID) -> Result<TimesheetWeekBatchDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let uid = claims.sub;
        let bid = parse_uuid(&id, "id")?;
        let m = timesheet_batch_service::approve_timesheet_week_batch(&db, tenant_id, bid, uid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(TimesheetWeekBatchDto::from(m))
    }

    async fn reject_timesheet_week_batch(
        &self,
        ctx: &Context<'_>,
        id: ID,
        rejection_reason: Option<String>,
    ) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let uid = claims.sub;
        let bid = parse_uuid(&id, "id")?;
        timesheet_batch_service::reject_timesheet_week_batch(&db, tenant_id, bid, uid, rejection_reason)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn upsert_attendance_adjustment_policy(
        &self,
        ctx: &Context<'_>,
        input: UpsertAttendanceAdjustmentPolicyInput,
    ) -> Result<AttendanceAdjustmentPolicyDto> {
        require_hrms_timesheet_settings(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let json = serde_json::to_string(&hrms_master_service::AttendanceAdjustmentPolicy {
            max_self_adjust_days: input.max_self_adjust_days,
        })
        .map_err(|e| KabiPayError::Validation(e.to_string()).into_graphql())?;
        hrms_master_service::upsert_policy_json(
            &db,
            tenant_id,
            hrms_master_service::CAT_ATTENDANCE_ADJUSTMENT,
            &json,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceAdjustmentPolicyDto {
            max_self_adjust_days: input.max_self_adjust_days,
        })
    }

    async fn upsert_timesheet_lock_policy(
        &self,
        ctx: &Context<'_>,
        input: UpsertTimesheetLockPolicyInput,
    ) -> Result<TimesheetLockPolicyDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_timesheet_configuration() {
            return Err(KabiPayError::Forbidden("timesheet:manage required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let json = serde_json::to_string(&hrms_master_service::TimesheetLockPolicy {
            editable_week_span: input.editable_week_span,
            lock_approved_entries: input.lock_approved_entries,
        })
        .map_err(|e| KabiPayError::Validation(e.to_string()).into_graphql())?;
        hrms_master_service::upsert_policy_json(&db, tenant_id, hrms_master_service::CAT_TIMESHEET_LOCK, &json)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(TimesheetLockPolicyDto {
            editable_week_span: input.editable_week_span,
            lock_approved_entries: input.lock_approved_entries,
        })
    }

    async fn upsert_timesheet_project(
        &self,
        ctx: &Context<'_>,
        code: String,
        name: String,
        #[graphql(default)] display_order: Option<i32>,
    ) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_timesheet_configuration() {
            return Err(KabiPayError::Forbidden("timesheet:manage required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let c = code.trim().to_uppercase();
        if c.is_empty() {
            return Err(KabiPayError::Validation("project code required".into()).into_graphql());
        }
        hrms_master_service::upsert_catalog_row(
            &db,
            tenant_id,
            hrms_master_service::CAT_TIMESHEET_PROJECT,
            &c,
            name.trim(),
            display_order,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn upsert_timesheet_task_types(
        &self,
        ctx: &Context<'_>,
        project_code: String,
        task_codes: Vec<String>,
    ) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_timesheet_configuration() {
            return Err(KabiPayError::Forbidden("timesheet:manage required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let pc = project_code.trim().to_uppercase();
        if pc.is_empty() {
            return Err(KabiPayError::Validation("projectCode required".into()).into_graphql());
        }
        let json = serde_json::to_string(&task_codes).map_err(|e| {
            KabiPayError::Validation(format!("task codes: {e}")).into_graphql()
        })?;
        hrms_master_service::upsert_catalog_row(
            &db,
            tenant_id,
            hrms_master_service::CAT_TIMESHEET_TASK,
            &pc,
            &json,
            Some(0),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// Replace per-employee allowed project codes (empty list clears restrictions — full catalog allowed).
    async fn set_employee_timesheet_projects(
        &self,
        ctx: &Context<'_>,
        employee_id: ID,
        project_codes: Vec<String>,
    ) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let target = parse_uuid(&employee_id, "employeeId")?;
        timesheet_assignment_auth::assert_can_write_employee_assignment_target(ctx, &db, tenant_id, target)
            .await?;
        let claims = require_client_claims(ctx)?;
        timesheet_project_assignment_service::set_assignments_for_employee(
            &db,
            tenant_id,
            target,
            project_codes,
            Some(claims.sub),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn upsert_holiday_calendar(
        &self,
        ctx: &Context<'_>,
        input: UpsertHolidayCalendarInput,
    ) -> Result<HolidayCalendarDto> {
        require_leave_configuration_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = input.id.as_ref().map(|i| parse_uuid(i, "calendarId")).transpose()?;
        let loc = input
            .location_id
            .as_ref()
            .map(|i| parse_uuid(i, "locationId"))
            .transpose()?;
        let m = attendance_service::upsert_holiday_calendar(
            &db,
            tenant_id,
            id,
            input.name,
            input.year,
            loc,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(HolidayCalendarDto::from(m))
    }

    async fn delete_holiday_calendar(&self, ctx: &Context<'_>, calendar_id: ID) -> Result<bool> {
        require_leave_configuration_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&calendar_id, "calendarId")?;
        let n = attendance_service::delete_holiday_calendar(&db, tenant_id, cid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(n > 0)
    }

    async fn upsert_holiday_day(
        &self,
        ctx: &Context<'_>,
        input: UpsertHolidayDayInput,
    ) -> Result<HolidayDayDto> {
        require_leave_configuration_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let calendar_id = parse_uuid(&input.calendar_id, "calendarId")?;
        let hid = input.id.as_ref().map(|i| parse_uuid(i, "holidayId")).transpose()?;
        let m = attendance_service::upsert_holiday_entry(
            &db,
            tenant_id,
            calendar_id,
            hid,
            input.holiday_date,
            input.name,
            input.holiday_type,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(HolidayDayDto::from(m))
    }

    async fn delete_holiday_day(&self, ctx: &Context<'_>, holiday_id: ID) -> Result<bool> {
        require_leave_configuration_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let hid = parse_uuid(&holiday_id, "holidayId")?;
        let n = attendance_service::delete_holiday_entry(&db, tenant_id, hid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(n > 0)
    }
}
