//! Write operations for attendance (punch in / out) and timesheet entries.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{data_scope_from_claims, resolve_viewer_employee},
    context::{
        ScopeType, PERM_ATTENDANCE_PUNCH_POLICY, PERM_ATTENDANCE_PUNCH_SELF,
        PERM_ATTENDANCE_REGULARIZE, PERM_LEAVE_MANAGE, PERM_TIMESHEET_APPROVE,
        PERM_TIMESHEET_MANAGE, PERM_TIMESHEET_WRITE,
    },
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

fn require_mutation_authority(
    ctx: &Context<'_>,
    permission: &'static str,
    allowed_scopes: &[ScopeType],
    require_employee_link: bool,
) -> Result<ScopeType> {
    let claims = require_client_claims(ctx)?;
    let scope = data_scope_from_claims(Some(claims), permission)
        .map_err(KabiPayError::into_graphql)?;
    if !allowed_scopes.contains(&scope) {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires one of these explicit scopes: {}",
            allowed_scopes
                .iter()
                .map(|scope| scope.to_wire())
                .collect::<Vec<_>>()
                .join(", ")
        ))
        .into_graphql());
    }
    if require_employee_link && claims.employee_id.is_none() {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission requires a JWT-linked employee"
        ))
        .into_graphql());
    }
    Ok(scope)
}

fn require_self_authority(ctx: &Context<'_>, permission: &'static str) -> Result<()> {
    require_mutation_authority(ctx, permission, &[ScopeType::Self_], true).map(|_| ())
}

fn require_team_or_all_authority(
    ctx: &Context<'_>,
    permission: &'static str,
) -> Result<ScopeType> {
    require_mutation_authority(ctx, permission, &[ScopeType::Team, ScopeType::All], false)
}

fn require_all_authority(ctx: &Context<'_>, permission: &'static str) -> Result<()> {
    require_mutation_authority(ctx, permission, &[ScopeType::All], false).map(|_| ())
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
        require_self_authority(ctx, PERM_ATTENDANCE_PUNCH_SELF)?;
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
        require_all_authority(ctx, PERM_ATTENDANCE_PUNCH_POLICY)?;
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
        require_self_authority(ctx, PERM_ATTENDANCE_PUNCH_SELF)?;
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
        require_self_authority(ctx, PERM_ATTENDANCE_PUNCH_SELF)?;
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
        require_team_or_all_authority(ctx, PERM_ATTENDANCE_REGULARIZE)?;
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
        require_team_or_all_authority(ctx, PERM_ATTENDANCE_REGULARIZE)?;
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
        require_self_authority(ctx, PERM_TIMESHEET_WRITE)?;
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
        require_self_authority(ctx, PERM_TIMESHEET_WRITE)?;
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
        require_self_authority(ctx, PERM_TIMESHEET_WRITE)?;
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
        require_self_authority(ctx, PERM_TIMESHEET_WRITE)?;
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

    async fn approve_timesheet_week_batch(
        &self,
        ctx: &Context<'_>,
        id: ID,
        expected_workflow_step_id: ID,
    ) -> Result<TimesheetWeekBatchDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let scope = require_team_or_all_authority(ctx, PERM_TIMESHEET_APPROVE)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope,
            permission: PERM_TIMESHEET_APPROVE,
        };
        let bid = parse_uuid(&id, "id")?;
        let expected_step_id =
            parse_uuid(&expected_workflow_step_id, "expectedWorkflowStepId")?;
        let m = timesheet_batch_service::approve_timesheet_week_batch(
            &db,
            tenant_id,
            bid,
            expected_step_id,
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
        expected_workflow_step_id: ID,
        rejection_reason: Option<String>,
    ) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let scope = require_team_or_all_authority(ctx, PERM_TIMESHEET_APPROVE)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope,
            permission: PERM_TIMESHEET_APPROVE,
        };
        let bid = parse_uuid(&id, "id")?;
        let expected_step_id =
            parse_uuid(&expected_workflow_step_id, "expectedWorkflowStepId")?;
        timesheet_batch_service::reject_timesheet_week_batch(
            &db,
            tenant_id,
            bid,
            expected_step_id,
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
        require_all_authority(ctx, PERM_ATTENDANCE_PUNCH_POLICY)?;
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
        require_all_authority(ctx, PERM_TIMESHEET_MANAGE)?;
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
        require_all_authority(ctx, PERM_TIMESHEET_MANAGE)?;
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
        require_all_authority(ctx, PERM_TIMESHEET_MANAGE)?;
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
        require_all_authority(ctx, PERM_TIMESHEET_MANAGE)?;
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
        require_all_authority(ctx, PERM_LEAVE_MANAGE)?;
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
        require_all_authority(ctx, PERM_LEAVE_MANAGE)?;
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
        require_all_authority(ctx, PERM_LEAVE_MANAGE)?;
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
        require_all_authority(ctx, PERM_LEAVE_MANAGE)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let hid = parse_uuid(&holiday_id, "holidayId")?;
        let n = attendance_service::delete_holiday_entry(&db, tenant_id, hid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(n > 0)
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use async_graphql::{EmptySubscription, Object, Request, Schema};
    use kabipay_common::{
        context::{
            ClientClaims, CLIENT_JWT_ISSUER, PERM_ATTENDANCE_PUNCH_POLICY,
            PERM_ATTENDANCE_PUNCH_SELF, PERM_ATTENDANCE_REGULARIZE, PERM_LEAVE_MANAGE,
            PERM_TIMESHEET_APPROVE, PERM_TIMESHEET_MANAGE, PERM_TIMESHEET_READ,
            PERM_TIMESHEET_WRITE,
        },
        subgraph::TenantId,
    };
    use std::collections::HashMap;

    struct TestQuery;

    #[Object]
    impl TestQuery {
        async fn api_version(&self) -> &str {
            "test"
        }
    }

    fn claims(
        permission: Option<&str>,
        scope: Option<&str>,
        employee_id: Option<Uuid>,
    ) -> ClientClaims {
        let permissions = permission.into_iter().map(str::to_owned).collect();
        let permission_scopes = permission
            .zip(scope)
            .map(|(permission, scope)| {
                HashMap::from([(permission.to_owned(), scope.to_owned())])
            })
            .unwrap_or_default();
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id,
            must_change_password: false,
            roles: vec![],
            permissions,
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    async fn execute_mutation(
        claims: ClientClaims,
        mutation: &str,
    ) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(TestQuery, MutationRoot, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(mutation))
            .await
    }

    fn mutation_inventory() -> Vec<(&'static str, String)> {
        let id = Uuid::new_v4();
        let step_id = Uuid::new_v4();
        vec![
            (PERM_ATTENDANCE_PUNCH_SELF, "mutation { punchToday { id } }".into()),
            (
                PERM_ATTENDANCE_PUNCH_POLICY,
                "mutation { upsertAttendancePunchPolicy(input: { isEnforced: false }) { id } }"
                    .into(),
            ),
            (
                PERM_ATTENDANCE_PUNCH_SELF,
                "mutation { addManualAttendanceSegment(input: { workDate: \"2026-08-27\", checkInTime: \"09:00:00\", checkOutTime: \"17:00:00\" }) { id } }".into(),
            ),
            (
                PERM_ATTENDANCE_PUNCH_SELF,
                format!("mutation {{ updateManualAttendanceSegment(input: {{ id: \"{id}\", workDate: \"2026-08-27\", checkInTime: \"09:00:00\", checkOutTime: \"17:00:00\" }}) {{ id }} }}"),
            ),
            (
                PERM_ATTENDANCE_REGULARIZE,
                format!("mutation {{ addManagedAttendanceSegment(input: {{ employeeId: \"{id}\", workDate: \"2026-08-27\", checkInTime: \"09:00:00\", checkOutTime: \"17:00:00\", reason: \"approved correction\" }}) {{ id }} }}"),
            ),
            (
                PERM_ATTENDANCE_REGULARIZE,
                format!("mutation {{ updateManagedAttendanceSegment(input: {{ id: \"{id}\", workDate: \"2026-08-27\", checkInTime: \"09:00:00\", checkOutTime: \"17:00:00\", reason: \"approved correction\", expectedUpdatedAt: \"2026-08-27T09:00:00Z\" }}) {{ id }} }}"),
            ),
            (
                PERM_TIMESHEET_WRITE,
                "mutation { createTimesheetEntry(input: { workDate: \"2026-08-27\", hoursWorked: \"8\" }) { id } }".into(),
            ),
            (
                PERM_TIMESHEET_WRITE,
                format!("mutation {{ deleteTimesheetEntry(id: \"{id}\") }}"),
            ),
            (
                PERM_TIMESHEET_WRITE,
                format!("mutation {{ updateTimesheetEntry(input: {{ id: \"{id}\", workDate: \"2026-08-27\", hoursWorked: \"8\" }}) {{ id }} }}"),
            ),
            (
                PERM_TIMESHEET_WRITE,
                "mutation { submitTimesheetWeek(weekStartDate: \"2026-08-24\") { id } }".into(),
            ),
            (
                PERM_TIMESHEET_APPROVE,
                format!("mutation {{ approveTimesheetWeekBatch(id: \"{id}\", expectedWorkflowStepId: \"{step_id}\") {{ id }} }}"),
            ),
            (
                PERM_TIMESHEET_APPROVE,
                format!("mutation {{ rejectTimesheetWeekBatch(id: \"{id}\", expectedWorkflowStepId: \"{step_id}\", rejectionReason: \"invalid\") }}"),
            ),
            (
                PERM_ATTENDANCE_PUNCH_POLICY,
                "mutation { upsertAttendanceAdjustmentPolicy(input: { maxSelfAdjustDays: 7 }) { maxSelfAdjustDays } }".into(),
            ),
            (
                PERM_TIMESHEET_MANAGE,
                "mutation { upsertTimesheetLockPolicy(input: { editableWeekSpan: 2, lockApprovedEntries: true }) { editableWeekSpan } }".into(),
            ),
            (
                PERM_TIMESHEET_MANAGE,
                "mutation { upsertTimesheetProject(code: \"P1\", name: \"Project\") }".into(),
            ),
            (
                PERM_TIMESHEET_MANAGE,
                "mutation { upsertTimesheetTaskTypes(projectCode: \"P1\", taskCodes: [\"DEV\"]) }".into(),
            ),
            (
                PERM_TIMESHEET_MANAGE,
                format!("mutation {{ setEmployeeTimesheetProjects(employeeId: \"{id}\", projectCodes: [\"P1\"]) }}"),
            ),
            (
                PERM_LEAVE_MANAGE,
                "mutation { upsertHolidayCalendar(input: { name: \"India\", year: 2026 }) { id } }".into(),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!("mutation {{ deleteHolidayCalendar(calendarId: \"{id}\") }}"),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!("mutation {{ upsertHolidayDay(input: {{ calendarId: \"{id}\", holidayDate: \"2026-08-15\", name: \"Holiday\" }}) {{ id }} }}"),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!("mutation {{ deleteHolidayDay(holidayId: \"{id}\") }}"),
            ),
        ]
    }

    fn allowed_scopes(permission: &str) -> &'static [&'static str] {
        match permission {
            PERM_ATTENDANCE_PUNCH_SELF | PERM_TIMESHEET_WRITE => &["SELF"],
            PERM_ATTENDANCE_REGULARIZE | PERM_TIMESHEET_APPROVE => &["TEAM", "ALL"],
            PERM_ATTENDANCE_PUNCH_POLICY | PERM_TIMESHEET_MANAGE | PERM_LEAVE_MANAGE => {
                &["ALL"]
            }
            _ => unreachable!("mutation inventory contains only attendance authorities"),
        }
    }

    fn assert_exact_permission_denied_before_db(
        response: &async_graphql::Response,
        permission: &str,
    ) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(permission),
            "expected exact {permission} denial, got: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    fn assert_authorization_reached_db(response: &async_graphql::Response, permission: &str) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            !message.contains(permission),
            "valid exact authority was rejected: {message}"
        );
        let code = response.errors[0]
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("code"))
            .cloned();
        assert_eq!(
            code,
            Some(async_graphql::Value::from("INTERNAL_ERROR")),
            "valid authority did not reach the tenant database boundary: {message}"
        );
    }

    #[tokio::test]
    async fn every_mutation_denies_missing_and_sibling_permissions_before_db_access() {
        for (required_permission, mutation) in mutation_inventory() {
            let employee_id = Some(Uuid::new_v4());
            let missing = execute_mutation(claims(None, None, employee_id), &mutation).await;
            assert_exact_permission_denied_before_db(&missing, required_permission);

            let sibling = execute_mutation(
                claims(Some(PERM_TIMESHEET_READ), Some("ALL"), employee_id),
                &mutation,
            )
            .await;
            assert_exact_permission_denied_before_db(&sibling, required_permission);
        }
    }

    #[tokio::test]
    async fn every_mutation_rejects_missing_malformed_or_unsuitable_exact_scope_before_db() {
        for (required_permission, mutation) in mutation_inventory() {
            for scope in [None, Some("INVALID"), Some("SELF"), Some("TEAM"), Some("DEPARTMENT"), Some("ALL")] {
                if scope.is_some_and(|scope| allowed_scopes(required_permission).contains(&scope)) {
                    continue;
                }
                let response = execute_mutation(
                    claims(Some(required_permission), scope, Some(Uuid::new_v4())),
                    &mutation,
                )
                .await;
                assert_exact_permission_denied_before_db(&response, required_permission);
            }
        }
    }

    #[tokio::test]
    async fn suitable_exact_scopes_allow_every_mutation_to_reach_its_database_boundary() {
        assert_eq!(mutation_inventory().len(), 21);
        for (required_permission, mutation) in mutation_inventory() {
            for scope in allowed_scopes(required_permission) {
                let response = execute_mutation(
                    claims(
                        Some(required_permission),
                        Some(scope),
                        Some(Uuid::new_v4()),
                    ),
                    &mutation,
                )
                .await;
                assert_authorization_reached_db(&response, required_permission);
            }
        }
    }

    #[tokio::test]
    async fn self_service_mutations_require_a_jwt_employee_link_before_db_access() {
        for (required_permission, mutation) in mutation_inventory().into_iter().filter(
            |(permission, _)| {
                matches!(
                    *permission,
                    PERM_ATTENDANCE_PUNCH_SELF | PERM_TIMESHEET_WRITE
                )
            },
        ) {
            let response = execute_mutation(
                claims(Some(required_permission), Some("SELF"), None),
                &mutation,
            )
            .await;
            assert_exact_permission_denied_before_db(&response, required_permission);
        }
    }

    #[test]
    fn timesheet_decisions_require_expected_workflow_step_id_in_graphql_schema() {
        let schema = Schema::build(TestQuery, MutationRoot, EmptySubscription).finish();
        let sdl = schema.sdl();

        assert!(sdl.contains(
            "approveTimesheetWeekBatch(id: ID!, expectedWorkflowStepId: ID!): TimesheetWeekBatch!"
        ), "unexpected SDL: {sdl}");
        assert!(sdl.contains(
            "rejectTimesheetWeekBatch(id: ID!, expectedWorkflowStepId: ID!, rejectionReason: String): Boolean!"
        ), "unexpected SDL: {sdl}");
    }
}
