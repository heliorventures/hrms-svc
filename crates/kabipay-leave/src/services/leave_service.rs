//! SeaORM-backed queries and commands for the leave domain. Every query applies the
//! `tenant_id` filter (defence in depth on top of schema isolation) and
//! the `is_deleted = false` soft-delete filter.

use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use kabipay_common::client_data_scope::{
    resolve_employee_scope_filter_with_connection, EmployeeScopeFilter,
};
use kabipay_common::context::{
    is_active_employment_status, ClientViewerEmployee, ScopeType, ACTIVE_EMPLOYMENT_STATUSES,
    PERM_LEAVE_APPROVE,
};
use kabipay_common::workflow_approval::{self, WorkflowApprovalAuthority};
use kabipay_common::workflow_inbox;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0010_time_shift_roster::{holiday, holiday_calendar};
use kabipay_db_entities::tenant::d0011_leave::{
    leave_balance, leave_policy, leave_request, leave_type,
};
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow, workflow_action, workflow_instance, workflow_step,
};
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait,
};
use std::collections::HashSet;
use uuid::Uuid;

use super::leave_admin;

pub async fn list_types(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<leave_type::Model>> {
    let limit = limit.clamp(1, 200);
    leave_type::Entity::find()
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .order_by_asc(leave_type::Column::Code)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Read existing leave balances only; provisioning belongs to employee/policy lifecycle writes.
pub async fn list_balances_for_employee(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    year: Option<i32>,
    limit: u64,
) -> KabiPayResult<Vec<leave_balance::Model>> {
    let limit = limit.clamp(1, 200);
    let mut q = leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(employee_id));
    if let Some(y) = year {
        q = q.filter(leave_balance::Column::Year.eq(y));
    }
    q.order_by_asc(leave_balance::Column::Year)
        .order_by_asc(leave_balance::Column::LeaveTypeId)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

fn inactive_leave_actor() -> KabiPayError {
    KabiPayError::Forbidden("leave actions require an active employee profile".into())
}

async fn resolve_leave_actor_candidate<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    actor_user_id: Uuid,
    claimed_employee_id: Option<Uuid>,
) -> KabiPayResult<employee::Model> {
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::UserId.eq(actor_user_id));
    if let Some(employee_id) = claimed_employee_id {
        query = query.filter(employee::Column::Id.eq(employee_id));
    }
    let actor = query
        .one(txn)
        .await?
        .ok_or_else(inactive_leave_actor)?;
    if actor.tenant_id != tenant_id
        || actor.user_id != Some(actor_user_id)
        || actor.is_deleted
        || claimed_employee_id.is_some_and(|claimed| claimed != actor.id)
        || !is_active_employment_status(&actor.status)
    {
        return Err(inactive_leave_actor());
    }
    Ok(actor)
}

fn employee_rows_for_update_query(
    mut employee_ids: Vec<Uuid>,
) -> sea_orm::Select<employee::Entity> {
    employee_ids.sort_unstable();
    employee_ids.dedup();
    employee::Entity::find()
        .filter(employee::Column::Id.is_in(employee_ids))
        .order_by_asc(employee::Column::Id)
        .lock_exclusive()
}

fn validate_locked_decision_employees(
    rows: Vec<employee::Model>,
    tenant_id: Uuid,
    actor_employee_id: Uuid,
    subject_employee_id: Uuid,
    actor_user_id: Uuid,
    claimed_actor_employee_id: Option<Uuid>,
) -> KabiPayResult<(ClientViewerEmployee, employee::Model)> {
    let actor = rows
        .iter()
        .find(|row| row.id == actor_employee_id)
        .ok_or_else(inactive_leave_actor)?;
    if actor.tenant_id != tenant_id
        || actor.is_deleted
        || actor.user_id != Some(actor_user_id)
        || claimed_actor_employee_id.is_some_and(|claimed| claimed != actor.id)
        || !is_active_employment_status(&actor.status)
    {
        return Err(inactive_leave_actor());
    }
    let subject = rows
        .iter()
        .find(|row| row.id == subject_employee_id)
        .cloned()
        .ok_or_else(|| KabiPayError::BusinessRule {
            code: "LEAVE_EMPLOYEE_INACTIVE",
            message: "leave decisions require an active employee".into(),
        })?;
    if subject.tenant_id != tenant_id
        || subject.is_deleted
        || !is_active_employment_status(&subject.status)
    {
        return Err(KabiPayError::BusinessRule {
            code: "LEAVE_EMPLOYEE_INACTIVE",
            message: "leave decisions require an active employee".into(),
        });
    }
    Ok((
        ClientViewerEmployee {
            employee_id: actor.id,
            department_id: actor.department_id,
        },
        subject,
    ))
}

async fn lock_and_validate_decision_employees<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    actor_employee_id: Uuid,
    subject_employee_id: Uuid,
    actor_user_id: Uuid,
    claimed_actor_employee_id: Option<Uuid>,
) -> KabiPayResult<(ClientViewerEmployee, employee::Model)> {
    let rows = employee_rows_for_update_query(vec![actor_employee_id, subject_employee_id])
        .all(txn)
        .await?;
    validate_locked_decision_employees(
        rows,
        tenant_id,
        actor_employee_id,
        subject_employee_id,
        actor_user_id,
        claimed_actor_employee_id,
    )
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct LeaveBalanceKey {
    tenant_id: Uuid,
    employee_id: Uuid,
    leave_type_id: Uuid,
    year: i32,
}

impl LeaveBalanceKey {
    fn advisory_key(self) -> String {
        format!(
            "leave_balance:{}:{}:{}:{}",
            self.tenant_id, self.employee_id, self.leave_type_id, self.year
        )
    }
}

fn leave_balance_for_update_query(key: LeaveBalanceKey) -> sea_orm::Select<leave_balance::Entity> {
    leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(key.tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(key.employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(key.leave_type_id))
        .filter(leave_balance::Column::Year.eq(key.year))
        .lock_exclusive()
}

async fn lock_leave_balance<C: ConnectionTrait + Sync>(
    txn: &C,
    key: LeaveBalanceKey,
) -> KabiPayResult<Option<leave_balance::Model>> {
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![key.advisory_key().into()],
    ))
    .await?;
    leave_balance_for_update_query(key)
        .one(txn)
        .await
        .map_err(KabiPayError::from)
}

async fn require_locked_leave_balance<C: ConnectionTrait + Sync>(
    txn: &C,
    key: LeaveBalanceKey,
) -> KabiPayResult<leave_balance::Model> {
    lock_leave_balance(txn, key)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_balance",
            id: format!(
                "{}-{}-{}",
                key.employee_id, key.leave_type_id, key.year
            ),
        })
}

#[derive(Clone, Copy, Debug)]
enum BalanceMovement {
    Reserve(Decimal),
    Approve(Decimal),
    Release(Decimal),
}

#[derive(Clone, Copy, Debug, PartialEq)]
struct BalanceAmounts {
    used: Decimal,
    pending: Decimal,
    available: Decimal,
}

fn apply_balance_movement(
    current: BalanceAmounts,
    movement: BalanceMovement,
) -> KabiPayResult<BalanceAmounts> {
    let days = match movement {
        BalanceMovement::Reserve(days)
        | BalanceMovement::Approve(days)
        | BalanceMovement::Release(days) => days,
    };
    if days <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "leave balance movement must be greater than zero".into(),
        ));
    }
    match movement {
        BalanceMovement::Reserve(days) if current.available < days => Err(
            KabiPayError::Validation("insufficient leave balance for this request".into()),
        ),
        BalanceMovement::Reserve(days) => Ok(BalanceAmounts {
            used: current.used,
            pending: current.pending + days,
            available: current.available - days,
        }),
        BalanceMovement::Approve(days) | BalanceMovement::Release(days)
            if current.pending < days =>
        {
            Err(KabiPayError::ConflictRule {
                code: "LEAVE_BALANCE_STATE_CONFLICT",
                message: "leave balance reservation is no longer consistent with the request"
                    .into(),
            })
        }
        BalanceMovement::Approve(days) => Ok(BalanceAmounts {
            used: current.used + days,
            pending: current.pending - days,
            available: current.available,
        }),
        BalanceMovement::Release(days) => Ok(BalanceAmounts {
            used: current.used,
            pending: current.pending - days,
            available: current.available + days,
        }),
    }
}

async fn move_locked_leave_balance<C: ConnectionTrait + Sync>(
    txn: &C,
    key: LeaveBalanceKey,
    movement: BalanceMovement,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<leave_balance::Model> {
    let row = require_locked_leave_balance(txn, key).await?;
    let next = apply_balance_movement(
        BalanceAmounts {
            used: row.used_days,
            pending: row.pending_days,
            available: row.balance_days,
        },
        movement,
    )?;
    let mut active: leave_balance::ActiveModel = row.into();
    active.used_days = Set(next.used);
    active.pending_days = Set(next.pending);
    active.balance_days = Set(next.available);
    active.updated_at = Set(now);
    active.update(txn).await.map_err(KabiPayError::from)
}

/// Submit a leave request in one transaction: validate leave type, require a
/// provisioned balance row, check remaining `balance_days`, insert `leave_request`
/// with status `PENDING`, and increase `pending_days` on the balance.
pub async fn submit_leave_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    actor_user_id: Uuid,
    claimed_employee_id: Option<Uuid>,
    leave_type_id: Uuid,
    from_date: NaiveDate,
    to_date: NaiveDate,
    is_half_day: bool,
    half_day_session: Option<String>,
    reason: Option<String>,
    supporting_document_reference: Option<String>,
) -> KabiPayResult<leave_request::Model> {
    if from_date > to_date {
        return Err(KabiPayError::Validation(
            "fromDate must be on or before toDate".into(),
        ));
    }

    let normalized_half_day_session = normalize_half_day_request(
        from_date,
        to_date,
        is_half_day,
        half_day_session,
    )?;

    let txn = db.begin().await?;
    let actor_candidate = resolve_leave_actor_candidate(
        &txn,
        tenant_id,
        actor_user_id,
        claimed_employee_id,
    )
    .await?;
    let (actor, _) = lock_and_validate_decision_employees(
        &txn,
        tenant_id,
        actor_candidate.id,
        actor_candidate.id,
        actor_user_id,
        claimed_employee_id,
    )
    .await?;
    let employee_id = actor.employee_id;

    // Prevent two simultaneous submissions from both passing the overlap
    // check before either row is visible. The key is tenant + employee scoped.
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("{tenant_id}:{employee_id}").into()],
    ))
    .await?;

    let overlapping = leave_request::Entity::find()
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::EmployeeId.eq(employee_id))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .filter(
            leave_request::Column::Status
                .is_in([STATUS_PENDING.to_string(), STATUS_APPROVED.to_string()]),
        )
        .filter(leave_request::Column::FromDate.lte(to_date))
        .filter(leave_request::Column::ToDate.gte(from_date))
        .all(&txn)
        .await?;
    if overlapping.iter().any(|existing| {
        leave_ranges_conflict(
            from_date,
            to_date,
            is_half_day,
            normalized_half_day_session.as_deref(),
            existing.from_date,
            existing.to_date,
            existing.is_half_day,
            existing.half_day_session.as_deref(),
        )
    }) {
        return Err(KabiPayError::BusinessRule {
            code: "LEAVE_DATE_OVERLAP",
            message: "An active leave request already covers all or part of this date range."
                .into(),
        });
    }

    let lt = leave_type::Entity::find_by_id(leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_type",
            id: leave_type_id.to_string(),
        })?;

    if is_half_day && !lt.half_day_allowed {
        return Err(KabiPayError::Validation(
            "this leave type does not allow half-day requests".into(),
        ));
    }

    let holiday_dates = tenant_holiday_dates_between(&txn, tenant_id, from_date, to_date).await?;

    let days = compute_requested_days(
        from_date,
        to_date,
        is_half_day,
        lt.sandwich_rule,
        &holiday_dates,
    )?;

    let doc_ref = supporting_document_reference
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty());
    if lt.requires_document && doc_ref.is_none() {
        return Err(KabiPayError::Validation(
            "this leave type requires a supporting document reference (link or reference ID)"
                .into(),
        ));
    }

    let req_id = Uuid::new_v4();
    let now = Utc::now();
    let (leave_workflow, first_step_id) =
        load_required_leave_workflow_first_step(&txn, tenant_id).await?;
    let am_req = leave_request::ActiveModel {
        id: Set(req_id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        leave_type_id: Set(leave_type_id),
        from_date: Set(from_date),
        to_date: Set(to_date),
        days_requested: Set(days),
        is_half_day: Set(is_half_day),
        half_day_session: Set(normalized_half_day_session),
        status: Set("PENDING".into()),
        reason: Set(reason),
        rejection_reason: Set(None),
        supporting_document_reference: Set(doc_ref),
        approved_by: Set(None),
        workflow_instance_id: Set(None),
        applied_at: Set(now),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am_req.insert(&txn).await?;

    attach_required_leave_workflow(
        &txn,
        tenant_id,
        req_id,
        leave_workflow.id,
        first_step_id,
        now,
    )
    .await?;

    if lt.is_paid {
        move_locked_leave_balance(
            &txn,
            LeaveBalanceKey {
                tenant_id,
                employee_id,
                leave_type_id,
                year: from_date.year(),
            },
            BalanceMovement::Reserve(days),
            now,
        )
        .await?;
    }

    let model = leave_request::Entity::find_by_id(req_id)
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted leave_request not found".into()))?;

    txn.commit().await?;
    Ok(model)
}

const STATUS_PENDING: &str = "PENDING";
const STATUS_APPROVED: &str = "APPROVED";
const STATUS_REJECTED: &str = "REJECTED";
const STATUS_CANCELLED: &str = "CANCELLED";

fn pending_request_for_decision_query(
    tenant_id: Uuid,
    request_id: Uuid,
    actor_employee_id: Uuid,
    allowed_employee_ids: Option<Vec<Uuid>>,
) -> sea_orm::Select<leave_request::Entity> {
    let mut query = leave_request::Entity::find_by_id(request_id)
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .filter(leave_request::Column::Status.eq(STATUS_PENDING))
        .filter(leave_request::Column::EmployeeId.ne(actor_employee_id));
    if let Some(employee_ids) = allowed_employee_ids {
        query = query.filter(leave_request::Column::EmployeeId.is_in(employee_ids));
    }
    query.lock_exclusive()
}

fn pending_request_for_decision_candidate_query(
    tenant_id: Uuid,
    request_id: Uuid,
    actor_employee_id: Uuid,
    allowed_employee_ids: Option<Vec<Uuid>>,
) -> sea_orm::Select<leave_request::Entity> {
    let mut query = leave_request::Entity::find_by_id(request_id)
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .filter(leave_request::Column::Status.eq(STATUS_PENDING))
        .filter(leave_request::Column::EmployeeId.ne(actor_employee_id));
    if let Some(employee_ids) = allowed_employee_ids {
        query = query.filter(leave_request::Column::EmployeeId.is_in(employee_ids));
    }
    query
}

fn pending_request_for_employee_query(
    tenant_id: Uuid,
    request_id: Uuid,
    employee_id: Uuid,
) -> sea_orm::Select<leave_request::Entity> {
    leave_request::Entity::find_by_id(request_id)
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::EmployeeId.eq(employee_id))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .filter(leave_request::Column::Status.eq(STATUS_PENDING))
        .lock_exclusive()
}

fn pending_leave_decision_unavailable() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "LEAVE_REQUEST_NOT_PENDING",
        message: "the leave request is not pending or was already decided".into(),
    }
}

fn leave_workflow_not_current() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "LEAVE_WORKFLOW_NOT_CURRENT",
        message: "the leave approval workflow is no longer at an actionable step".into(),
    }
}

pub async fn resolve_leave_pending_approval_stage(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: &str,
    workflow_instance_id: Option<Uuid>,
) -> KabiPayResult<Option<String>> {
    workflow_inbox::pending_workflow_step_title(
        db,
        tenant_id,
        status,
        STATUS_PENDING,
        workflow_instance_id,
    )
    .await
}

/// Returns the current workflow step only when it is actionable by this exact viewer now.
pub async fn resolve_actionable_leave_workflow_step_id(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    status: &str,
    subject_employee_id: Uuid,
    workflow_instance_id: Option<Uuid>,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<Option<Uuid>> {
    if !status.trim().eq_ignore_ascii_case(STATUS_PENDING) {
        return Ok(None);
    }
    let Some(actor) = authority.actor_employee else {
        return Ok(None);
    };
    if actor.employee_id == subject_employee_id {
        return Ok(None);
    }
    let Some(instance_id) = workflow_instance_id else {
        return Ok(None);
    };
    let actor_row = employee::Entity::find_by_id(actor.employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::UserId.eq(authority.actor_user_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await?;
    if !actor_row
        .as_ref()
        .is_some_and(|row| is_active_employment_status(&row.status))
    {
        return Ok(None);
    }
    let subject_row = employee::Entity::find_by_id(subject_employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await?;
    if !subject_row
        .as_ref()
        .is_some_and(|row| is_active_employment_status(&row.status))
    {
        return Ok(None);
    }
    let Some(instance) = actionable_workflow_instance_query(
        instance_id,
        tenant_id,
        request_id,
    )
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(current_step_id) = instance.current_step_id else {
        return Ok(None);
    };
    let Some(_workflow) = actionable_workflow_query(instance.workflow_id, tenant_id)
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    let Some(step) = actionable_workflow_step_query(
        current_step_id,
        tenant_id,
        instance.workflow_id,
    )
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if workflow_approval::assert_workflow_step_actor(
        db,
        tenant_id,
        subject_employee_id,
        &step,
        authority,
    )
    .await
    .is_err()
    {
        return Ok(None);
    }
    Ok(Some(step.id))
}

fn actionable_workflow_instance_query(
    instance_id: Uuid,
    tenant_id: Uuid,
    request_id: Uuid,
) -> sea_orm::Select<workflow_instance::Entity> {
    workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq(WF_ENTITY_LEAVE))
        .filter(workflow_instance::Column::EntityId.eq(request_id))
        .filter(workflow_instance::Column::Status.eq(WF_STATUS_IN_PROGRESS))
}

fn actionable_workflow_query(
    workflow_id: Uuid,
    tenant_id: Uuid,
) -> sea_orm::Select<workflow::Entity> {
    workflow::Entity::find_by_id(workflow_id)
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_LEAVE))
}

fn actionable_workflow_step_query(
    step_id: Uuid,
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> sea_orm::Select<workflow_step::Entity> {
    workflow_step::Entity::find_by_id(step_id)
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
}

fn normalize_half_day_request(
    from_date: NaiveDate,
    to_date: NaiveDate,
    is_half_day: bool,
    half_day_session: Option<String>,
) -> KabiPayResult<Option<String>> {
    if !is_half_day {
        return Ok(None);
    }
    if from_date != to_date {
        return Err(KabiPayError::Validation(
            "half-day leave must start and end on the same date".into(),
        ));
    }
    let normalized = half_day_session
        .map(|value| value.trim().to_ascii_uppercase())
        .unwrap_or_default();
    if !matches!(normalized.as_str(), "FIRST_HALF" | "SECOND_HALF") {
        return Err(KabiPayError::Validation(
            "halfDaySession must be FIRST_HALF or SECOND_HALF".into(),
        ));
    }
    Ok(Some(normalized))
}

fn leave_ranges_conflict(
    from_date: NaiveDate,
    to_date: NaiveDate,
    is_half_day: bool,
    half_day_session: Option<&str>,
    existing_from_date: NaiveDate,
    existing_to_date: NaiveDate,
    existing_is_half_day: bool,
    existing_half_day_session: Option<&str>,
) -> bool {
    if existing_to_date < from_date || existing_from_date > to_date {
        return false;
    }

    let complementary_half_days = is_half_day
        && existing_is_half_day
        && from_date == to_date
        && existing_from_date == existing_to_date
        && existing_from_date == from_date
        && half_day_session.is_some()
        && existing_half_day_session.is_some()
        && half_day_session != existing_half_day_session;
    !complementary_half_days
}

#[cfg(test)]
mod overlap_tests {
    use super::leave_ranges_conflict;
    use chrono::NaiveDate;

    fn date(day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(2026, 8, day).expect("valid test date")
    }

    #[test]
    fn full_day_requests_conflict_on_the_same_date() {
        assert!(leave_ranges_conflict(
            date(17),
            date(17),
            false,
            None,
            date(17),
            date(17),
            false,
            None,
        ));
    }

    #[test]
    fn complementary_half_day_sessions_do_not_conflict() {
        assert!(!leave_ranges_conflict(
            date(17),
            date(17),
            true,
            Some("SECOND_HALF"),
            date(17),
            date(17),
            true,
            Some("FIRST_HALF"),
        ));
    }

    #[test]
    fn duplicate_half_day_sessions_conflict() {
        assert!(leave_ranges_conflict(
            date(17),
            date(17),
            true,
            Some("FIRST_HALF"),
            date(17),
            date(17),
            true,
            Some("FIRST_HALF"),
        ));
    }

    #[test]
    fn non_overlapping_ranges_do_not_conflict_even_when_called_directly() {
        assert!(!leave_ranges_conflict(
            date(17),
            date(17),
            false,
            None,
            date(18),
            date(18),
            false,
            None,
        ));
    }
}

/// New outbox rows start here until a consumer marks them processed (Gap G — M6).
const OUTBOX_STATUS_PENDING: &str = "PENDING";

/// Matches `workflow.entity_type` / `workflow_instance.entity_type` for leave (seed + M8).
const WF_ENTITY_LEAVE: &str = "LEAVE_REQUEST";
const WF_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
const WF_STATUS_COMPLETED: &str = "COMPLETED";
const WF_STATUS_CANCELLED: &str = "CANCELLED";
const WF_STATUS_REJECTED: &str = "REJECTED";
const WF_ACTION_APPROVE: &str = "APPROVE";
const WF_ACTION_REJECT: &str = "REJECT";

async fn load_required_leave_workflow_first_step<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
) -> KabiPayResult<(workflow::Model, Uuid)> {
    let wf = active_leave_workflow_query(tenant_id)
        .one(txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(leave_workflow_not_current)?;
    let step = first_leave_workflow_step_query(tenant_id, wf.id)
        .one(txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(leave_workflow_not_current)?;
    Ok((wf, step.id))
}

fn active_leave_workflow_query(tenant_id: Uuid) -> sea_orm::Select<workflow::Entity> {
    workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::IsActive.eq(true))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_LEAVE))
        .order_by_asc(workflow::Column::Name)
        .lock_exclusive()
}

fn first_leave_workflow_step_query(
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> sea_orm::Select<workflow_step::Entity> {
    workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .lock_exclusive()
}

async fn attach_required_leave_workflow<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    leave_request_id: Uuid,
    workflow_id: Uuid,
    first_step_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let inst_id = Uuid::new_v4();
    let inst = workflow_instance::ActiveModel {
        id: Set(inst_id),
        tenant_id: Set(tenant_id),
        workflow_id: Set(workflow_id),
        entity_type: Set(WF_ENTITY_LEAVE.into()),
        entity_id: Set(leave_request_id),
        status: Set(WF_STATUS_IN_PROGRESS.into()),
        current_step_id: Set(Some(first_step_id)),
        created_at: Set(now),
        completed_at: Set(None),
        updated_at: Set(now),
    };
    inst.insert(txn).await.map_err(KabiPayError::from)?;

    let mut am_req: leave_request::ActiveModel =
        leave_request::Entity::find_by_id(leave_request_id)
            .one(txn)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("leave_request missing after insert".into()))?
            .into();
    am_req.workflow_instance_id = Set(Some(inst_id));
    am_req.update(txn).await.map_err(KabiPayError::from)?;
    Ok(())
}

async fn request_uses_leave_balance(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    model: &leave_request::Model,
) -> KabiPayResult<bool> {
    let lt = leave_type::Entity::find_by_id(model.leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_type",
            id: model.leave_type_id.to_string(),
        })?;
    Ok(lt.is_paid)
}

async fn lock_current_leave_workflow<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    model: &leave_request::Model,
    expected_workflow_step_id: Uuid,
) -> KabiPayResult<(workflow_instance::Model, workflow_step::Model)> {
    let instance_id = require_workflow_instance_id(model.workflow_instance_id)?;
    let instance = workflow_instance_for_decision_query(
        instance_id,
        tenant_id,
        model.id,
    )
        .one(txn)
        .await?
        .ok_or_else(leave_workflow_not_current)?;
    require_expected_workflow_step(instance.current_step_id, expected_workflow_step_id)?;
    workflow_for_decision_query(instance.workflow_id, tenant_id)
        .one(txn)
        .await?
        .ok_or_else(leave_workflow_not_current)?;
    let step = workflow_step_for_decision_query(
        expected_workflow_step_id,
        tenant_id,
        instance.workflow_id,
    )
        .one(txn)
        .await?
        .ok_or_else(leave_workflow_not_current)?;
    Ok((instance, step))
}

fn workflow_instance_for_decision_query(
    instance_id: Uuid,
    tenant_id: Uuid,
    request_id: Uuid,
) -> sea_orm::Select<workflow_instance::Entity> {
    workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq(WF_ENTITY_LEAVE))
        .filter(workflow_instance::Column::EntityId.eq(request_id))
        .filter(workflow_instance::Column::Status.eq(WF_STATUS_IN_PROGRESS))
        .lock_exclusive()
}

fn workflow_step_for_decision_query(
    step_id: Uuid,
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> sea_orm::Select<workflow_step::Entity> {
    workflow_step::Entity::find_by_id(step_id)
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
        .lock_exclusive()
}

fn workflow_for_decision_query(
    workflow_id: Uuid,
    tenant_id: Uuid,
) -> sea_orm::Select<workflow::Entity> {
    workflow::Entity::find_by_id(workflow_id)
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_LEAVE))
        .lock_exclusive()
}

fn require_expected_workflow_step(
    current_step_id: Option<Uuid>,
    expected_workflow_step_id: Uuid,
) -> KabiPayResult<()> {
    if current_step_id == Some(expected_workflow_step_id) {
        Ok(())
    } else {
        Err(leave_workflow_not_current())
    }
}

fn require_workflow_instance_id(instance_id: Option<Uuid>) -> KabiPayResult<Uuid> {
    instance_id.ok_or_else(leave_workflow_not_current)
}

/// Final approval: leave row APPROVED, paid-leave balance movement when applicable, and outbox event.
async fn finalize_leave_approval(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    model: &leave_request::Model,
    approver_user_id: Uuid,
    now: chrono::DateTime<Utc>,
    request_id: Uuid,
) -> KabiPayResult<()> {
    let mut am_req: leave_request::ActiveModel = model.clone().into();
    am_req.status = Set(STATUS_APPROVED.into());
    am_req.rejection_reason = Set(None);
    am_req.approved_by = Set(Some(approver_user_id));
    am_req.updated_at = Set(now);
    am_req.update(txn).await?;

    let out = leave_request::Entity::find_by_id(request_id)
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated leave_request not found".into()))?;

    let payload = serde_json::json!({
        "schema_version": 1,
        "leave_request_id": out.id,
        "employee_id": out.employee_id,
        "leave_type_id": out.leave_type_id,
        "approver_user_id": approver_user_id,
        "from_date": out.from_date.to_string(),
        "to_date": out.to_date.to_string(),
        "days_requested": out.days_requested.normalize().to_string(),
        "is_half_day": out.is_half_day,
        "status": out.status,
    });
    let ob = outbox_event::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        aggregate_type: Set("leave_request".into()),
        aggregate_id: Set(request_id),
        event_type: Set("leave_request.approved".into()),
        payload: Set(payload),
        status: Set(OUTBOX_STATUS_PENDING.into()),
        retry_count: Set(0),
        last_error: Set(None),
        created_at: Set(now),
        processed_at: Set(None),
        claimed_at: Set(None),
    };
    ob.insert(txn).await?;
    Ok(())
}

/// Set request to APPROVED, `approved_by` = `approver_user_id` (user.id), and move
/// `pending_days` → `used_days` on the annual balance (submit already reserved balance).
///
/// When **`workflow_instance_id`** is set (**M8**), records **`workflow_action`**, advances
/// **`workflow_instance.current_step_id`** until the last step; only the **final** step
/// performs balance movement and emits **`outbox_event`** (same as M6).
pub async fn approve_leave_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    expected_workflow_step_id: Uuid,
    actor_user_id: Uuid,
    claimed_actor_employee_id: Option<Uuid>,
    scope: ScopeType,
) -> KabiPayResult<leave_request::Model> {
    let txn = db.begin().await?;
    let actor_candidate = resolve_leave_actor_candidate(
        &txn,
        tenant_id,
        actor_user_id,
        claimed_actor_employee_id,
    )
    .await?;
    let candidate_actor = ClientViewerEmployee {
        employee_id: actor_candidate.id,
        department_id: actor_candidate.department_id,
    };
    let candidate_scope_filter = resolve_employee_scope_filter_with_connection(
        &txn,
        tenant_id,
        scope,
        Some(candidate_actor),
    )
    .await?;
    let candidate_request = load_scoped_pending_request_candidate(
        &txn,
        tenant_id,
        request_id,
        candidate_actor.employee_id,
        candidate_scope_filter,
    )
    .await?;
    let (actor, subject) = lock_and_validate_decision_employees(
        &txn,
        tenant_id,
        candidate_actor.employee_id,
        candidate_request.employee_id,
        actor_user_id,
        claimed_actor_employee_id,
    )
    .await?;
    let scope_filter = resolve_employee_scope_filter_with_connection(
        &txn,
        tenant_id,
        scope,
        Some(actor),
    )
    .await?;
    let model = load_scoped_pending_request_in_txn(
        &txn,
        tenant_id,
        request_id,
        actor.employee_id,
        scope_filter,
    )
    .await?;
    if model.employee_id != subject.id {
        return Err(pending_leave_decision_unavailable());
    }
    let authority = WorkflowApprovalAuthority {
        actor_user_id,
        actor_employee: Some(actor),
        scope,
        permission: PERM_LEAVE_APPROVE,
    };
    let now = Utc::now();
    let (inst, cur_step) = lock_current_leave_workflow(
        &txn,
        tenant_id,
        &model,
        expected_workflow_step_id,
    )
    .await?;
    let inst_id = inst.id;
    let cur_step_id = cur_step.id;

    workflow_approval::assert_workflow_step_actor(
        &txn,
        tenant_id,
        model.employee_id,
        &cur_step,
        &authority,
    )
    .await?;

    let act = workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        instance_id: Set(inst_id),
        workflow_step_id: Set(cur_step_id),
        performed_by: Set(Some(actor_user_id)),
        action: Set(WF_ACTION_APPROVE.into()),
        remarks: Set(None),
        acted_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    };
    act.insert(&txn).await?;

    let next_step = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(inst.workflow_id))
        .filter(workflow_step::Column::SequenceOrder.gt(cur_step.sequence_order))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .lock_exclusive()
        .one(&txn)
        .await?;

    if let Some(next) = next_step {
        let mut am_inst: workflow_instance::ActiveModel = inst.into();
        am_inst.current_step_id = Set(Some(next.id));
        am_inst.updated_at = Set(now);
        am_inst.update(&txn).await?;
        txn.commit().await?;
        return leave_request::Entity::find_by_id(request_id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("leave_request missing after commit".into()));
    }

    let mut am_inst: workflow_instance::ActiveModel = inst.into();
    am_inst.status = Set(WF_STATUS_COMPLETED.into());
    am_inst.current_step_id = Set(None);
    am_inst.completed_at = Set(Some(now));
    am_inst.updated_at = Set(now);
    am_inst.update(&txn).await?;

    if request_uses_leave_balance(&txn, tenant_id, &model).await? {
        move_locked_leave_balance(
            &txn,
            LeaveBalanceKey {
                tenant_id,
                employee_id: model.employee_id,
                leave_type_id: model.leave_type_id,
                year: model.from_date.year(),
            },
            BalanceMovement::Approve(model.days_requested),
            now,
        )
        .await?;
    }
    finalize_leave_approval(
        &txn,
        tenant_id,
        &model,
        actor_user_id,
        now,
        request_id,
    )
    .await?;

    txn.commit().await?;
    let out = leave_request::Entity::find_by_id(request_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("leave_request missing after commit".into()))?;

    leave_notify_employee(
        db,
        tenant_id,
        out.employee_id,
        "Leave approved",
        "Your leave request was approved.",
    )
    .await;
    Ok(out)
}

/// Reject a PENDING request, release the balance hold, and optionally record a reason.
/// Cancels an in-progress **`workflow_instance`** when **`workflow_instance_id`** is set (**M8**).
pub async fn reject_leave_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    expected_workflow_step_id: Uuid,
    actor_user_id: Uuid,
    claimed_actor_employee_id: Option<Uuid>,
    scope: ScopeType,
    rejection_reason: Option<String>,
) -> KabiPayResult<leave_request::Model> {
    let txn = db.begin().await?;
    let actor_candidate = resolve_leave_actor_candidate(
        &txn,
        tenant_id,
        actor_user_id,
        claimed_actor_employee_id,
    )
    .await?;
    let candidate_actor = ClientViewerEmployee {
        employee_id: actor_candidate.id,
        department_id: actor_candidate.department_id,
    };
    let candidate_scope_filter = resolve_employee_scope_filter_with_connection(
        &txn,
        tenant_id,
        scope,
        Some(candidate_actor),
    )
    .await?;
    let candidate_request = load_scoped_pending_request_candidate(
        &txn,
        tenant_id,
        request_id,
        candidate_actor.employee_id,
        candidate_scope_filter,
    )
    .await?;
    let (actor, subject) = lock_and_validate_decision_employees(
        &txn,
        tenant_id,
        candidate_actor.employee_id,
        candidate_request.employee_id,
        actor_user_id,
        claimed_actor_employee_id,
    )
    .await?;
    let scope_filter = resolve_employee_scope_filter_with_connection(
        &txn,
        tenant_id,
        scope,
        Some(actor),
    )
    .await?;
    let model = load_scoped_pending_request_in_txn(
        &txn,
        tenant_id,
        request_id,
        actor.employee_id,
        scope_filter,
    )
    .await?;
    if model.employee_id != subject.id {
        return Err(pending_leave_decision_unavailable());
    }
    let authority = WorkflowApprovalAuthority {
        actor_user_id,
        actor_employee: Some(actor),
        scope,
        permission: PERM_LEAVE_APPROVE,
    };
    let now = Utc::now();
    let (inst, step) = lock_current_leave_workflow(
        &txn,
        tenant_id,
        &model,
        expected_workflow_step_id,
    )
    .await?;
    workflow_approval::assert_workflow_step_actor(
        &txn,
        tenant_id,
        model.employee_id,
        &step,
        &authority,
    )
    .await?;
    workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        instance_id: Set(inst.id),
        workflow_step_id: Set(step.id),
        performed_by: Set(Some(actor_user_id)),
        action: Set(WF_ACTION_REJECT.into()),
        remarks: Set(rejection_reason.clone()),
        acted_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    let mut active_instance: workflow_instance::ActiveModel = inst.into();
    active_instance.status = Set(WF_STATUS_REJECTED.into());
    active_instance.current_step_id = Set(None);
    active_instance.completed_at = Set(Some(now));
    active_instance.updated_at = Set(now);
    active_instance.update(&txn).await?;

    if request_uses_leave_balance(&txn, tenant_id, &model).await? {
        move_locked_leave_balance(
            &txn,
            LeaveBalanceKey {
                tenant_id,
                employee_id: model.employee_id,
                leave_type_id: model.leave_type_id,
                year: model.from_date.year(),
            },
            BalanceMovement::Release(model.days_requested),
            now,
        )
        .await?;
    }

    let mut am_req: leave_request::ActiveModel = model.clone().into();
    am_req.status = Set(STATUS_REJECTED.into());
    am_req.rejection_reason = Set(rejection_reason);
    am_req.approved_by = Set(None);
    am_req.updated_at = Set(now);
    am_req.update(&txn).await?;

    let out = leave_request::Entity::find_by_id(request_id)
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated leave_request not found".into()))?;
    txn.commit().await?;
    let msg = match &out.rejection_reason {
        Some(s) if !s.is_empty() => format!("Your leave was rejected. Reason: {s}"),
        _ => "Your leave request was rejected.".into(),
    };
    leave_notify_employee(db, tenant_id, out.employee_id, "Leave rejected", &msg).await;
    Ok(out)
}

/// Withdraw a **PENDING** request by the submitting employee: cancel workflow if any,
/// release balance reservation, set status `CANCELLED`.
pub async fn cancel_leave_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    actor_user_id: Uuid,
    claimed_actor_employee_id: Option<Uuid>,
) -> KabiPayResult<leave_request::Model> {
    let txn = db.begin().await?;
    let actor_candidate = resolve_leave_actor_candidate(
        &txn,
        tenant_id,
        actor_user_id,
        claimed_actor_employee_id,
    )
    .await?;
    let (actor, _) = lock_and_validate_decision_employees(
        &txn,
        tenant_id,
        actor_candidate.id,
        actor_candidate.id,
        actor_user_id,
        claimed_actor_employee_id,
    )
    .await?;
    let model = pending_request_for_employee_query(tenant_id, request_id, actor.employee_id)
        .one(&txn)
        .await?
        .ok_or_else(pending_leave_decision_unavailable)?;
    let now = Utc::now();
    let instance_id = require_workflow_instance_id(model.workflow_instance_id)?;
    let instance = workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq(WF_ENTITY_LEAVE))
        .filter(workflow_instance::Column::EntityId.eq(model.id))
        .filter(workflow_instance::Column::Status.eq(WF_STATUS_IN_PROGRESS))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(leave_workflow_not_current)?;
    let mut active_instance: workflow_instance::ActiveModel = instance.into();
    active_instance.status = Set(WF_STATUS_CANCELLED.into());
    active_instance.current_step_id = Set(None);
    active_instance.completed_at = Set(Some(now));
    active_instance.updated_at = Set(now);
    active_instance.update(&txn).await?;

    if request_uses_leave_balance(&txn, tenant_id, &model).await? {
        move_locked_leave_balance(
            &txn,
            LeaveBalanceKey {
                tenant_id,
                employee_id: model.employee_id,
                leave_type_id: model.leave_type_id,
                year: model.from_date.year(),
            },
            BalanceMovement::Release(model.days_requested),
            now,
        )
        .await?;
    }

    let mut am_req: leave_request::ActiveModel = model.clone().into();
    am_req.status = Set(STATUS_CANCELLED.into());
    am_req.rejection_reason = Set(None);
    am_req.approved_by = Set(None);
    am_req.updated_at = Set(now);
    am_req.update(&txn).await?;

    let out = leave_request::Entity::find_by_id(request_id)
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated leave_request not found".into()))?;
    txn.commit().await?;
    leave_notify_employee(
        db,
        tenant_id,
        out.employee_id,
        "Leave withdrawn",
        "Your pending leave request was cancelled.",
    )
    .await;
    Ok(out)
}

async fn load_scoped_pending_request_in_txn<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    request_id: Uuid,
    actor_employee_id: Uuid,
    scope_filter: EmployeeScopeFilter,
) -> KabiPayResult<leave_request::Model> {
    let allowed_employee_ids = match scope_filter {
        EmployeeScopeFilter::Unrestricted => None,
        EmployeeScopeFilter::Empty => return Err(pending_leave_decision_unavailable()),
        EmployeeScopeFilter::EmployeeIds(employee_ids) => Some(employee_ids),
    };
    pending_request_for_decision_query(
        tenant_id,
        request_id,
        actor_employee_id,
        allowed_employee_ids,
    )
        .one(txn)
        .await?
        .ok_or_else(pending_leave_decision_unavailable)
}

async fn load_scoped_pending_request_candidate<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    request_id: Uuid,
    actor_employee_id: Uuid,
    scope_filter: EmployeeScopeFilter,
) -> KabiPayResult<leave_request::Model> {
    let allowed_employee_ids = match scope_filter {
        EmployeeScopeFilter::Unrestricted => None,
        EmployeeScopeFilter::Empty => return Err(pending_leave_decision_unavailable()),
        EmployeeScopeFilter::EmployeeIds(employee_ids) => Some(employee_ids),
    };
    pending_request_for_decision_candidate_query(
        tenant_id,
        request_id,
        actor_employee_id,
        allowed_employee_ids,
    )
    .one(txn)
    .await?
    .ok_or_else(pending_leave_decision_unavailable)
}

fn balance_days_from_components(
    entitled: Decimal,
    carried: Decimal,
    used: Decimal,
    pending: Decimal,
) -> KabiPayResult<Decimal> {
    let available = entitled + carried - used - pending;
    if available < Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "balance_days would be negative; check entitled, carried forward, used, and pending"
                .into(),
        ));
    }
    Ok(available)
}

async fn validate_balance_references<C: ConnectionTrait + Sync>(
    txn: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    leave_type_id: Uuid,
) -> KabiPayResult<()> {
    employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        })?;
    leave_type::Entity::find_by_id(leave_type_id)
        .filter(leave_type::Column::TenantId.eq(tenant_id))
        .filter(leave_type::Column::IsDeleted.eq(false))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_type",
            id: leave_type_id.to_string(),
        })?;
    Ok(())
}

async fn set_locked_leave_balance<C: ConnectionTrait + Sync>(
    txn: &C,
    key: LeaveBalanceKey,
    entitled_days: Decimal,
    used_days: Decimal,
    pending_days: Decimal,
    carried_forward_days: Decimal,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<leave_balance::Model> {
    let balance_days = balance_days_from_components(
        entitled_days,
        carried_forward_days,
        used_days,
        pending_days,
    )?;
    if let Some(row) = lock_leave_balance(txn, key).await? {
        let mut active: leave_balance::ActiveModel = row.into();
        active.entitled_days = Set(entitled_days);
        active.used_days = Set(used_days);
        active.pending_days = Set(pending_days);
        active.carried_forward_days = Set(carried_forward_days);
        active.balance_days = Set(balance_days);
        active.updated_at = Set(now);
        return active.update(txn).await.map_err(KabiPayError::from);
    }
    leave_balance::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(key.tenant_id),
        employee_id: Set(key.employee_id),
        leave_type_id: Set(key.leave_type_id),
        year: Set(key.year),
        entitled_days: Set(entitled_days),
        used_days: Set(used_days),
        pending_days: Set(pending_days),
        carried_forward_days: Set(carried_forward_days),
        balance_days: Set(balance_days),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(txn)
    .await
    .map_err(KabiPayError::from)
}

pub async fn upsert_leave_balance(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    leave_type_id: Uuid,
    year: i32,
    entitled_days: Decimal,
    used_days: Decimal,
    pending_days: Decimal,
    carried_forward_days: Decimal,
) -> KabiPayResult<leave_balance::Model> {
    let txn = db.begin().await?;
    validate_balance_references(&txn, tenant_id, employee_id, leave_type_id).await?;
    let model = set_locked_leave_balance(
        &txn,
        LeaveBalanceKey {
            tenant_id,
            employee_id,
            leave_type_id,
            year,
        },
        entitled_days,
        used_days,
        pending_days,
        carried_forward_days,
        Utc::now(),
    )
    .await?;
    txn.commit().await?;
    Ok(model)
}

pub async fn adjust_leave_balance_entitlement(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    leave_type_id: Uuid,
    year: i32,
    entitled_delta: Decimal,
) -> KabiPayResult<leave_balance::Model> {
    let txn = db.begin().await?;
    let key = LeaveBalanceKey {
        tenant_id,
        employee_id,
        leave_type_id,
        year,
    };
    let row = require_locked_leave_balance(&txn, key).await?;
    let entitled = row.entitled_days + entitled_delta;
    if entitled < Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "entitled_days cannot go negative".into(),
        ));
    }
    let balance = balance_days_from_components(
        entitled,
        row.carried_forward_days,
        row.used_days,
        row.pending_days,
    )?;
    let mut active: leave_balance::ActiveModel = row.into();
    active.entitled_days = Set(entitled);
    active.balance_days = Set(balance);
    active.updated_at = Set(Utc::now());
    let model = active.update(&txn).await?;
    txn.commit().await?;
    Ok(model)
}

pub async fn provision_leave_balances_from_policies(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    year: i32,
) -> KabiPayResult<u32> {
    let txn = db.begin().await?;
    let policies = leave_policy::Entity::find()
        .filter(leave_policy::Column::TenantId.eq(tenant_id))
        .order_by_asc(leave_policy::Column::LeaveTypeId)
        .order_by_asc(leave_policy::Column::Id)
        .limit(500)
        .all(&txn)
        .await?;
    let mut seen_types = HashSet::new();
    let policies: Vec<_> = policies
        .into_iter()
        .filter(|policy| seen_types.insert(policy.leave_type_id))
        .collect();
    let employees = employees_for_leave_provisioning_query(tenant_id)
        .all(&txn)
        .await?;
    let now = Utc::now();
    let as_of = now.date_naive();
    let mut touched = 0_u32;
    for employee in employees {
        for policy in &policies {
            if let Some(applicable_to) = policy.applicable_to.as_deref() {
                let applicable_to = applicable_to.trim().to_ascii_uppercase();
                if !applicable_to.is_empty() && applicable_to != "ALL" && applicable_to != "*" {
                    continue;
                }
            }
            let Some(entitled) = leave_admin::entitled_days_from_policy_as_of(
                policy,
                Some(employee.date_of_joining),
                year,
                as_of,
            ) else {
                continue;
            };
            if entitled <= Decimal::ZERO {
                continue;
            }
            let key = LeaveBalanceKey {
                tenant_id,
                employee_id: employee.id,
                leave_type_id: policy.leave_type_id,
                year,
            };
            let existing = lock_leave_balance(&txn, key).await?;
            let (used, pending, carried) = existing
                .as_ref()
                .map(|row| {
                    (
                        row.used_days,
                        row.pending_days,
                        row.carried_forward_days,
                    )
                })
                .unwrap_or((Decimal::ZERO, Decimal::ZERO, Decimal::ZERO));
            let expected_balance =
                balance_days_from_components(entitled, carried, used, pending)?;
            if existing.as_ref().is_some_and(|row| {
                row.entitled_days == entitled && row.balance_days == expected_balance
            }) {
                continue;
            }
            if let Some(row) = existing {
                let mut active: leave_balance::ActiveModel = row.into();
                active.entitled_days = Set(entitled);
                active.balance_days = Set(expected_balance);
                active.updated_at = Set(now);
                active.update(&txn).await?;
            } else {
                leave_balance::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    tenant_id: Set(tenant_id),
                    employee_id: Set(employee.id),
                    leave_type_id: Set(policy.leave_type_id),
                    year: Set(year),
                    entitled_days: Set(entitled),
                    used_days: Set(Decimal::ZERO),
                    pending_days: Set(Decimal::ZERO),
                    carried_forward_days: Set(Decimal::ZERO),
                    balance_days: Set(expected_balance),
                    created_at: Set(now),
                    updated_at: Set(now),
                }
                .insert(&txn)
                .await?;
            }
            touched += 1;
        }
    }
    txn.commit().await?;
    Ok(touched)
}

fn employees_for_leave_provisioning_query(
    tenant_id: Uuid,
) -> sea_orm::Select<employee::Entity> {
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(
            employee::Column::Status.is_in(
                ACTIVE_EMPLOYMENT_STATUSES.map(str::to_owned),
            ),
        )
        .order_by_asc(employee::Column::Id)
}

pub async fn leave_workflow_action_trail(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    workflow_instance_id: Uuid,
) -> KabiPayResult<Vec<(workflow_action::Model, String)>> {
    let acts = workflow_action::Entity::find()
        .filter(workflow_action::Column::TenantId.eq(tenant_id))
        .filter(workflow_action::Column::InstanceId.eq(workflow_instance_id))
        .order_by_asc(workflow_action::Column::ActedAt)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut out = Vec::with_capacity(acts.len());
    for a in acts {
        let step_name = workflow_step::Entity::find_by_id(a.workflow_step_id)
            .filter(workflow_step::Column::TenantId.eq(tenant_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .map(|s| s.step_name)
            .unwrap_or_else(|| "(unknown step)".into());
        out.push((a, step_name));
    }
    Ok(out)
}

/// Best-effort in-app row for the requester's linked `user` (if any).
async fn leave_notify_employee(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    title: &str,
    message: &str,
) {
    let user_id: Option<Uuid> = match employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
    {
        Ok(Some(emp)) => emp.user_id,
        _ => None,
    };
    let Some(user_id) = user_id else {
        return;
    };
    let now = Utc::now();
    let am = notification::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        r#type: Set(Some("LEAVE".into())),
        title: Set(Some(title.into())),
        message: Set(Some(message.into())),
        action_url: Set(None),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    if let Err(e) = am.insert(db).await {
        tracing::warn!(error = %e, "insert notification (leave) failed");
    }
}

async fn tenant_holiday_dates_between<C: ConnectionTrait + Sync>(
    conn: &C,
    tenant_id: Uuid,
    from_date: NaiveDate,
    to_date: NaiveDate,
) -> KabiPayResult<HashSet<NaiveDate>> {
    let calendars = holiday_calendar::Entity::find()
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .all(conn)
        .await
        .map_err(KabiPayError::from)?;

    let calendar_ids: Vec<Uuid> = calendars.into_iter().map(|c| c.id).collect();
    if calendar_ids.is_empty() {
        return Ok(HashSet::new());
    }

    let rows = holiday::Entity::find()
        .filter(holiday::Column::CalendarId.is_in(calendar_ids))
        .filter(holiday::Column::HolidayDate.between(from_date, to_date))
        .all(conn)
        .await
        .map_err(KabiPayError::from)?;

    Ok(rows.into_iter().map(|h| h.holiday_date).collect())
}

fn compute_requested_days(
    from_date: NaiveDate,
    to_date: NaiveDate,
    is_half_day: bool,
    sandwich_rule: bool,
    holidays: &HashSet<NaiveDate>,
) -> KabiPayResult<Decimal> {
    if is_half_day {
        if from_date != to_date {
            return Err(KabiPayError::Validation(
                "half-day leave must have the same fromDate and toDate".into(),
            ));
        }
        return Ok(Decimal::new(5, 1));
    }

    if sandwich_rule {
        let n = (to_date - from_date).num_days() + 1;
        if n < 1 {
            return Err(KabiPayError::Validation(
                "fromDate must be on or before toDate".into(),
            ));
        }
        return Ok(Decimal::from(n));
    }

    let mut count: i64 = 0;
    let mut cur = from_date;
    while cur <= to_date {
        let wd = cur.weekday();
        if wd != Weekday::Sat && wd != Weekday::Sun && !holidays.contains(&cur) {
            count += 1;
        }
        cur += Duration::days(1);
    }

    if count == 0 {
        return Err(KabiPayError::Validation(
            "no chargeable working days in this date range (weekends and holidays are excluded when sandwich rule is off)"
                .into(),
        ));
    }

    Ok(Decimal::from(count))
}

#[cfg(test)]
mod decision_authorization_tests {
    use super::*;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, QueryTrait,
    };
    use std::collections::BTreeMap;
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct RecordingProxy {
        rows: Vec<ProxyRow>,
        statements: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for RecordingProxy {
        async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.statements
                .lock()
                .expect("proxy statement recorder")
                .push(format!("QUERY {}", statement));
            Ok(self.rows.clone())
        }

        async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
            self.statements
                .lock()
                .expect("proxy statement recorder")
                .push(format!("EXECUTE {}", statement));
            Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected: 0,
            })
        }
    }

    async fn proxy_connection(
        rows: Vec<ProxyRow>,
    ) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
        let statements = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(RecordingProxy {
                rows,
                statements: Arc::clone(&statements),
            })),
        )
        .await
        .expect("PostgreSQL proxy connection");
        (db, statements)
    }

    #[test]
    fn pending_decision_query_embeds_scope_self_exclusion_and_row_lock() {
        let tenant_id = Uuid::parse_str("e6d4fc13-feb8-52a0-93bd-f66c795969b1").unwrap();
        let request_id = Uuid::parse_str("7cb9cb55-7ab4-4e20-a084-924b0a2cbb91").unwrap();
        let actor_id = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let subject_id = Uuid::parse_str("22222222-2222-4222-8222-222222222222").unwrap();

        let sql = pending_request_for_decision_query(
            tenant_id,
            request_id,
            actor_id,
            Some(vec![subject_id]),
        )
            .build(DbBackend::Postgres)
            .to_string();

        assert!(sql.contains("\"id\" = '7cb9cb55-7ab4-4e20-a084-924b0a2cbb91'"));
        assert!(sql.contains("\"tenant_id\" = 'e6d4fc13-feb8-52a0-93bd-f66c795969b1'"));
        assert!(sql.contains("\"is_deleted\" = FALSE"));
        assert!(sql.contains("\"status\" = 'PENDING'"));
        assert!(sql.contains("\"employee_id\" <> '11111111-1111-4111-8111-111111111111'"));
        assert!(sql.contains("\"employee_id\" IN ('22222222-2222-4222-8222-222222222222')"));
        assert!(sql.ends_with("FOR UPDATE"));

        let own_sql = pending_request_for_employee_query(tenant_id, request_id, subject_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(own_sql.contains("\"employee_id\" = '22222222-2222-4222-8222-222222222222'"));
        assert!(own_sql.ends_with("FOR UPDATE"));
    }

    #[tokio::test]
    async fn empty_scope_rejects_hidden_request_without_querying_or_locking_it() {
        let (db, statements) = proxy_connection(vec![]).await;
        let error = load_scoped_pending_request_in_txn(
            &db,
            Uuid::new_v4(),
            Uuid::new_v4(),
            Uuid::new_v4(),
            EmployeeScopeFilter::Empty,
        )
        .await
        .expect_err("empty scope must hide the request");
        assert_eq!(error.code(), "LEAVE_REQUEST_NOT_PENDING");
        assert!(statements.lock().expect("statement recorder").is_empty());
    }

    #[tokio::test]
    async fn recursive_team_scope_includes_descendants_while_all_is_unrestricted() {
        let manager = Uuid::new_v4();
        let direct = Uuid::new_v4();
        let indirect = Uuid::new_v4();
        let rows = [manager, direct, indirect]
            .into_iter()
            .map(|id| ProxyRow::new(BTreeMap::from([("id".into(), id.into())])))
            .collect();
        let (db, statements) = proxy_connection(rows).await;
        let tenant_id = Uuid::new_v4();
        let viewer = ClientViewerEmployee {
            employee_id: manager,
            department_id: None,
        };
        let team = resolve_employee_scope_filter_with_connection(
            &db,
            tenant_id,
            ScopeType::Team,
            Some(viewer),
        )
        .await
        .expect("recursive team scope");
        assert!(team.allows_employee(direct));
        assert!(team.allows_employee(indirect));
        let sql = statements.lock().expect("statement recorder")[0].clone();
        assert!(sql.contains("WITH RECURSIVE team"));
        assert!(sql.contains("child.status IN"));
        assert!(sql.contains("PROBATION"));

        let all = resolve_employee_scope_filter_with_connection(
            &db,
            tenant_id,
            ScopeType::All,
            Some(viewer),
        )
        .await
        .expect("ALL scope");
        assert!(matches!(all, EmployeeScopeFilter::Unrestricted));
        assert_eq!(statements.lock().expect("statement recorder").len(), 1);

        let missing_manager = resolve_employee_scope_filter_with_connection(
            &db,
            tenant_id,
            ScopeType::Team,
            None,
        )
        .await
        .expect("missing manager identity fails closed");
        assert!(matches!(missing_manager, EmployeeScopeFilter::Empty));
        assert_eq!(statements.lock().expect("statement recorder").len(), 1);
    }

    fn employee_row(
        id: Uuid,
        tenant_id: Uuid,
        user_id: Option<Uuid>,
        status: &str,
    ) -> employee::Model {
        let now = Utc::now();
        employee::Model {
            id,
            tenant_id,
            user_id,
            department_id: None,
            designation_id: None,
            cost_center_id: None,
            location_id: None,
            reporting_manager_id: None,
            employee_code: format!("EMP-{id}"),
            first_name: "Test".into(),
            last_name: "Employee".into(),
            date_of_birth: None,
            gender: None,
            blood_group: None,
            nationality: None,
            employment_type: Some("FULL_TIME".into()),
            status: status.into(),
            date_of_joining: NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
            probation_end_date: None,
            notice_period_days: None,
            emergency_contact_name: None,
            emergency_contact_phone: None,
            emergency_contact_relation: None,
            personal_phone: None,
            current_address: None,
            permanent_address: None,
            uan_number: None,
            esic_number: None,
            is_deleted: false,
            deleted_at: None,
            deleted_by: None,
            created_at: now,
            updated_at: now,
        }
    }

    fn employee_proxy_row(model: &employee::Model) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("user_id".into(), model.user_id.into()),
            ("department_id".into(), model.department_id.into()),
            ("designation_id".into(), model.designation_id.into()),
            ("cost_center_id".into(), model.cost_center_id.into()),
            ("location_id".into(), model.location_id.into()),
            ("reporting_manager_id".into(), model.reporting_manager_id.into()),
            ("employee_code".into(), model.employee_code.clone().into()),
            ("first_name".into(), model.first_name.clone().into()),
            ("last_name".into(), model.last_name.clone().into()),
            ("date_of_birth".into(), model.date_of_birth.into()),
            ("gender".into(), model.gender.clone().into()),
            ("blood_group".into(), model.blood_group.clone().into()),
            ("nationality".into(), model.nationality.clone().into()),
            ("employment_type".into(), model.employment_type.clone().into()),
            ("status".into(), model.status.clone().into()),
            ("date_of_joining".into(), model.date_of_joining.into()),
            ("probation_end_date".into(), model.probation_end_date.into()),
            ("notice_period_days".into(), model.notice_period_days.into()),
            ("emergency_contact_name".into(), model.emergency_contact_name.clone().into()),
            ("emergency_contact_phone".into(), model.emergency_contact_phone.clone().into()),
            ("emergency_contact_relation".into(), model.emergency_contact_relation.clone().into()),
            ("personal_phone".into(), model.personal_phone.clone().into()),
            ("current_address".into(), model.current_address.clone().into()),
            ("permanent_address".into(), model.permanent_address.clone().into()),
            ("uan_number".into(), model.uan_number.clone().into()),
            ("esic_number".into(), model.esic_number.clone().into()),
            ("is_deleted".into(), model.is_deleted.into()),
            ("deleted_at".into(), model.deleted_at.into()),
            ("deleted_by".into(), model.deleted_by.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    #[tokio::test]
    async fn inactive_actor_is_rejected_before_scope_or_request_reads() {
        let tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        let actor_user_id = Uuid::new_v4();
        let inactive = employee_row(actor_id, tenant_id, Some(actor_user_id), "INACTIVE");
        let (inactive_db, inactive_statements) =
            proxy_connection(vec![employee_proxy_row(&inactive)]).await;

        let inactive_error = resolve_leave_actor_candidate(
            &inactive_db,
            tenant_id,
            actor_user_id,
            Some(actor_id),
        )
        .await
        .expect_err("inactive actor must fail immediately after actor lookup");

        let (missing_db, missing_statements) = proxy_connection(Vec::new()).await;
        let missing_error = resolve_leave_actor_candidate(
            &missing_db,
            tenant_id,
            actor_user_id,
            Some(actor_id),
        )
        .await
        .expect_err("missing actor must fail at the same boundary");

        assert_eq!(inactive_error.code(), missing_error.code());
        assert_eq!(inactive_error.code(), "FORBIDDEN");
        assert_eq!(inactive_statements.lock().expect("statement recorder").len(), 1);
        assert_eq!(missing_statements.lock().expect("statement recorder").len(), 1);
    }

    #[test]
    fn decision_employee_rows_lock_in_uuid_order_before_status_or_identity_validation() {
        let first = Uuid::parse_str("11111111-1111-4111-8111-111111111111").unwrap();
        let second = Uuid::parse_str("eeeeeeee-eeee-4eee-8eee-eeeeeeeeeeee").unwrap();
        let sql = employee_rows_for_update_query(vec![second, first])
            .build(DbBackend::Postgres)
            .to_string();
        assert!(sql.find(&first.to_string()).unwrap() < sql.find(&second.to_string()).unwrap());
        assert!(sql.contains("ORDER BY \"employee\".\"id\" ASC"));
        assert!(sql.ends_with("FOR UPDATE"));
        assert!(!sql.contains("\"status\" ="));
        assert!(!sql.contains("\"status\" IN"));
        assert!(!sql.contains("\"tenant_id\" ="));
    }

    #[test]
    fn locked_decision_employees_accept_probation_and_validate_actor_binding() {
        let tenant_id = Uuid::new_v4();
        let actor_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let actor_user_id = Uuid::new_v4();
        let rows = vec![
            employee_row(subject_id, tenant_id, Some(Uuid::new_v4()), "ACTIVE"),
            employee_row(actor_id, tenant_id, Some(actor_user_id), "PROBATION"),
        ];
        let (actor, subject) = validate_locked_decision_employees(
            rows.clone(),
            tenant_id,
            actor_id,
            subject_id,
            actor_user_id,
            Some(actor_id),
        )
        .expect("active and probation employees are valid");
        assert_eq!(actor.employee_id, actor_id);
        assert_eq!(subject.id, subject_id);

        let mut inactive_rows = rows;
        inactive_rows
            .iter_mut()
            .find(|row| row.id == subject_id)
            .expect("subject")
            .status = "INACTIVE".into();
        let error = validate_locked_decision_employees(
            inactive_rows,
            tenant_id,
            actor_id,
            subject_id,
            actor_user_id,
            Some(actor_id),
        )
        .expect_err("inactive subject must fail after rows are locked");
        assert_eq!(error.code(), "LEAVE_EMPLOYEE_INACTIVE");
    }

    #[tokio::test]
    async fn balance_lock_uses_advisory_key_before_exact_row_for_update() {
        let (db, statements) = proxy_connection(vec![]).await;
        let key = LeaveBalanceKey {
            tenant_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(),
            leave_type_id: Uuid::new_v4(),
            year: 2026,
        };
        let row = lock_leave_balance(&db, key).await.expect("balance lock");
        assert!(row.is_none());
        let statements = statements.lock().expect("statement recorder");
        assert_eq!(statements.len(), 2);
        assert!(statements[0].contains("pg_advisory_xact_lock"));
        assert!(statements[0].contains(&key.advisory_key()));
        assert!(statements[1].contains(&key.tenant_id.to_string()));
        assert!(statements[1].contains(&key.employee_id.to_string()));
        assert!(statements[1].contains(&key.leave_type_id.to_string()));
        assert!(statements[1].contains("2026"));
        assert!(statements[1].ends_with("FOR UPDATE"));
    }

    #[test]
    fn sequential_balance_transitions_never_partially_consume_a_reservation() {
        let initial = BalanceAmounts {
            used: Decimal::ZERO,
            pending: Decimal::ZERO,
            available: Decimal::new(30, 1),
        };
        let after_first = apply_balance_movement(
            initial,
            BalanceMovement::Reserve(Decimal::new(20, 1)),
        )
        .expect("first reservation");
        let second = apply_balance_movement(
            after_first,
            BalanceMovement::Reserve(Decimal::new(20, 1)),
        )
        .expect_err("the second serialized request exceeds the remaining balance");
        assert_eq!(second.code(), "VALIDATION_ERROR");
        let approved = apply_balance_movement(
            after_first,
            BalanceMovement::Approve(Decimal::new(20, 1)),
        )
        .expect("final approval consumes the full reservation");
        assert_eq!(approved.used, Decimal::new(20, 1));
        assert_eq!(approved.pending, Decimal::ZERO);
        assert_eq!(approved.available, Decimal::new(10, 1));

        let inconsistent = apply_balance_movement(
            BalanceAmounts {
                used: Decimal::ZERO,
                pending: Decimal::new(10, 1),
                available: Decimal::new(10, 1),
            },
            BalanceMovement::Release(Decimal::new(20, 1)),
        )
        .expect_err("release cannot partially consume a stale reservation");
        assert_eq!(inconsistent.code(), "LEAVE_BALANCE_STATE_CONFLICT");
    }

    #[test]
    fn workflow_queries_bind_request_instance_step_and_expected_current_step() {
        let tenant_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let expected_step_id = Uuid::new_v4();
        let instance_sql = workflow_instance_for_decision_query(
            instance_id,
            tenant_id,
            request_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(instance_sql.contains("\"entity_type\" = 'LEAVE_REQUEST'"));
        assert!(instance_sql.contains(&request_id.to_string()));
        assert!(instance_sql.contains("\"status\" = 'IN_PROGRESS'"));
        assert!(instance_sql.ends_with("FOR UPDATE"));
        let step_sql = workflow_step_for_decision_query(
            expected_step_id,
            tenant_id,
            workflow_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(step_sql.contains(&workflow_id.to_string()));
        assert!(step_sql.ends_with("FOR UPDATE"));
        let workflow_sql = workflow_for_decision_query(workflow_id, tenant_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(workflow_sql.contains("\"entity_type\" = 'LEAVE_REQUEST'"));
        assert!(workflow_sql.contains(&tenant_id.to_string()));
        assert!(workflow_sql.ends_with("FOR UPDATE"));

        let workflowless = require_workflow_instance_id(None)
            .expect_err("legacy workflow-less requests cannot be decided directly");
        assert_eq!(workflowless.code(), "LEAVE_WORKFLOW_NOT_CURRENT");
        assert!(require_expected_workflow_step(Some(expected_step_id), expected_step_id).is_ok());
        for current in [None, Some(Uuid::new_v4())] {
            let error = require_expected_workflow_step(current, expected_step_id)
                .expect_err("workflow-less, stale, or duplicate actions must fail");
            assert_eq!(error.code(), "LEAVE_WORKFLOW_NOT_CURRENT");
        }
        let advanced_step = Uuid::new_v4();
        let duplicate = require_expected_workflow_step(Some(advanced_step), expected_step_id)
            .expect_err("duplicate intermediate approval uses the stale previous step");
        assert_eq!(duplicate.code(), "LEAVE_WORKFLOW_NOT_CURRENT");
    }

    #[tokio::test]
    async fn submission_requires_an_active_leave_workflow_and_first_step() {
        let tenant_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let workflow_sql = active_leave_workflow_query(tenant_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(workflow_sql.contains("\"entity_type\" = 'LEAVE_REQUEST'"));
        assert!(workflow_sql.contains("\"is_active\" = TRUE"));
        assert!(workflow_sql.ends_with("FOR UPDATE"));
        let step_sql = first_leave_workflow_step_query(tenant_id, workflow_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(step_sql.contains(&workflow_id.to_string()));
        assert!(step_sql.contains("ORDER BY"));
        assert!(step_sql.ends_with("FOR UPDATE"));

        let (db, statements) = proxy_connection(vec![]).await;
        let error = load_required_leave_workflow_first_step(&db, tenant_id)
            .await
            .expect_err("submission cannot proceed without an active leave workflow");
        assert_eq!(error.code(), "LEAVE_WORKFLOW_NOT_CURRENT");
        assert_eq!(statements.lock().expect("statement recorder").len(), 1);
    }

    #[test]
    fn leave_provisioning_uses_the_canonical_active_employment_statuses() {
        let sql = employees_for_leave_provisioning_query(Uuid::new_v4())
            .build(DbBackend::Postgres)
            .to_string();
        assert!(sql.contains("'ACTIVE'"));
        assert!(sql.contains("'PROBATION'"));
        assert!(sql.contains("\"is_deleted\" = FALSE"));
        assert!(sql.contains("ORDER BY \"employee\".\"id\" ASC"));
    }

    #[tokio::test]
    async fn actionable_step_returns_none_before_database_access_when_not_actionable() {
        let actor_id = Uuid::new_v4();
        let authority = WorkflowApprovalAuthority {
            actor_user_id: Uuid::new_v4(),
            actor_employee: Some(ClientViewerEmployee {
                employee_id: actor_id,
                department_id: None,
            }),
            scope: ScopeType::All,
            permission: PERM_LEAVE_APPROVE,
        };
        for (status, subject_id, instance_id) in [
            ("APPROVED", Uuid::new_v4(), Some(Uuid::new_v4())),
            ("PENDING", actor_id, Some(Uuid::new_v4())),
            ("PENDING", Uuid::new_v4(), None),
        ] {
            let step = resolve_actionable_leave_workflow_step_id(
                &DatabaseConnection::Disconnected,
                Uuid::new_v4(),
                Uuid::new_v4(),
                status,
                subject_id,
                instance_id,
                &authority,
            )
            .await
            .expect("non-actionable rows resolve uniformly to None");
            assert_eq!(step, None);
        }
    }

    #[test]
    fn actionable_step_queries_bind_instance_workflow_and_step_to_the_leave_request() {
        let tenant_id = Uuid::new_v4();
        let request_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let step_id = Uuid::new_v4();
        let instance_sql = actionable_workflow_instance_query(
            instance_id,
            tenant_id,
            request_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(instance_sql.contains(&request_id.to_string()));
        assert!(instance_sql.contains("\"entity_type\" = 'LEAVE_REQUEST'"));
        assert!(instance_sql.contains("\"status\" = 'IN_PROGRESS'"));
        let workflow_sql = actionable_workflow_query(workflow_id, tenant_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(workflow_sql.contains(&workflow_id.to_string()));
        assert!(workflow_sql.contains("\"entity_type\" = 'LEAVE_REQUEST'"));
        let step_sql = actionable_workflow_step_query(step_id, tenant_id, workflow_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(step_sql.contains(&step_id.to_string()));
        assert!(step_sql.contains(&workflow_id.to_string()));
    }

    #[test]
    fn stale_or_repeated_decisions_share_a_stable_conflict_code() {
        let error = pending_leave_decision_unavailable();
        assert_eq!(error.code(), "LEAVE_REQUEST_NOT_PENDING");
    }

}
