//! Write operations for attendance (punch in / out) and timesheet entries.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{data_scope_from_context, resolve_viewer_employee},
    context::PERM_TIMESHEET_APPROVE,
    subgraph::{
        client_request_hints, require_client_claims, require_tenant_id, resolve_client_employee_id,
        ops_db, tenant_db,
    },
    tenant_business_clock::TenantBusinessClock,
    workflow_approval::WorkflowApprovalAuthority,
    KabiPayError,
};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee, d0010_time_shift_roster::attendance,
};
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, TransactionTrait};
use uuid::Uuid;

use crate::resolvers::types::{
    AddManagedAttendanceSegmentInput, AddManualAttendanceSegmentInput,
    AttendanceAdjustmentPolicyDto, AttendanceDto, AttendancePunchPolicyDto,
    CreateTimesheetEntryInput, HolidayCalendarDto, HolidayDayDto, ManagedAttendanceDto,
    PunchTodayInput, TimesheetEntryDto, TimesheetLockPolicyDto, TimesheetWeekBatchDto,
    UpdateManagedAttendanceSegmentInput, UpdateManualAttendanceSegmentInput,
    UpdateTimesheetEntryInput, UpsertAttendanceAdjustmentPolicyInput,
    UpsertAttendancePunchPolicyInput, UpsertHolidayCalendarInput, UpsertHolidayDayInput,
    UpsertTimesheetLockPolicyInput,
};
use crate::resolvers::attendance_management_auth;
use crate::resolvers::timesheet_assignment_auth;
use crate::services::{
    attendance_management_service::ManagedAttendanceRow,
    attendance_regularization_service::{
        create_managed_attendance_segment_in_transaction,
        update_managed_attendance_segment_in_transaction, ManagedCreateCommand,
        ManagedUpdateCommand, SegmentTimes,
    },
    attendance_service, hrms_master_service, punch_policy, timesheet_batch_service,
    timesheet_project_assignment_service,
};

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

async fn managed_attendance_dto<C>(
    db: &C,
    row: attendance::Model,
) -> Result<ManagedAttendanceDto>
where
    C: ConnectionTrait,
{
    let target = employee::Entity::find_by_id(row.employee_id)
        .filter(employee::Column::TenantId.eq(row.tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| {
            KabiPayError::Internal("managed attendance employee missing after write".into())
                .into_graphql()
        })?;
    Ok(ManagedAttendanceDto::from(ManagedAttendanceRow {
        employee_name: format!("{} {}", target.first_name, target.last_name)
            .trim()
            .to_owned(),
        employee_code: target.employee_code,
        attendance: row,
    }))
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
                    "attendance:punch_self permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
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
        let m = attendance_service::punch_today(
            &db,
            tenant_id,
            employee_id,
            clock,
            geo,
            client_ip,
        )
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
                "attendance punch policy permission is required".into(),
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
                    "attendance:punch_self permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let m = attendance_service::add_manual_attendance_segment(
            &db,
            tenant_id,
            employee_id,
            clock,
            input.work_date,
            input.check_in_time,
            input.check_out_time,
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
                    "attendance:punch_self permission required".into(),
                )
                .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let attendance_id = parse_uuid(&input.id, "id")?;
        let m = attendance_service::update_manual_attendance_segment(
            &db,
            tenant_id,
            attendance_id,
            employee_id,
            clock,
            input.work_date,
            input.check_in_time,
            input.check_out_time,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AttendanceDto::from(m))
    }

    async fn add_managed_attendance_segment(
        &self,
        ctx: &Context<'_>,
        input: AddManagedAttendanceSegmentInput,
    ) -> Result<ManagedAttendanceDto> {
        let tenant_id = require_tenant_id(ctx)?;
        attendance_management_auth::require_regularizer(ctx)?;
        let actor_user_id = require_client_claims(ctx)?.sub;
        let target_employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        let request_id = client_request_hints(ctx).request_id;
        let db = tenant_db(ctx, tenant_id).await?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let segment = SegmentTimes::for_manual_input(
            input.work_date,
            input.check_in_time,
            input.check_out_time,
        );
        let instants = segment.to_instants(clock).map_err(KabiPayError::into_graphql)?;
        let mut txn = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        attendance_management_auth::assert_target_in_scope_with_connection(
            ctx,
            &txn,
            tenant_id,
            target_employee_id,
        )
        .await?;
        let created = create_managed_attendance_segment_in_transaction(
            &mut txn,
            &ManagedCreateCommand {
                tenant_id,
                target_employee_id,
                actor_user_id,
                segment,
                instants,
                today: clock.now_date(),
                reason: input.reason,
                request_id,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let result = managed_attendance_dto(&txn, created).await?;
        txn.commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(result)
    }

    async fn update_managed_attendance_segment(
        &self,
        ctx: &Context<'_>,
        input: UpdateManagedAttendanceSegmentInput,
    ) -> Result<ManagedAttendanceDto> {
        let tenant_id = require_tenant_id(ctx)?;
        attendance_management_auth::require_regularizer(ctx)?;
        let actor_user_id = require_client_claims(ctx)?.sub;
        let attendance_id = parse_uuid(&input.id, "id")?;
        let request_id = client_request_hints(ctx).request_id;
        let db = tenant_db(ctx, tenant_id).await?;
        let clock = TenantBusinessClock::load(ops_db(ctx)?, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let segment = SegmentTimes::for_manual_input(
            input.work_date,
            input.check_in_time,
            input.check_out_time,
        );
        let instants = segment.to_instants(clock).map_err(KabiPayError::into_graphql)?;
        let mut txn = db
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let initial = attendance_management_auth::attendance_target_in_scope_with_connection(
            ctx,
            &txn,
            tenant_id,
            attendance_id,
        )
        .await?;
        let updated = update_managed_attendance_segment_in_transaction(
            &mut txn,
            &ManagedUpdateCommand {
                tenant_id,
                attendance_id,
                target_employee_id: initial.employee_id,
                actor_user_id,
                initial_work_date: initial.work_date,
                segment,
                instants,
                today: clock.now_date(),
                reason: input.reason,
                request_id,
                expected_updated_at: input.expected_updated_at,
            },
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        let result = managed_attendance_dto(&txn, updated).await?;
        txn.commit()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(result)
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
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope: data_scope_from_context(ctx, PERM_TIMESHEET_APPROVE)?,
            permission: PERM_TIMESHEET_APPROVE,
        };
        let bid = parse_uuid(&id, "id")?;
        let m = timesheet_batch_service::approve_timesheet_week_batch(
            &db,
            tenant_id,
            bid,
            &authority,
        )
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
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope: data_scope_from_context(ctx, PERM_TIMESHEET_APPROVE)?,
            permission: PERM_TIMESHEET_APPROVE,
        };
        let bid = parse_uuid(&id, "id")?;
        timesheet_batch_service::reject_timesheet_week_batch(
            &db,
            tenant_id,
            bid,
            &authority,
            rejection_reason,
        )
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
