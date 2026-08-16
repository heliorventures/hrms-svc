//! SeaORM-backed queries and commands for the leave domain. Every query applies the
//! `tenant_id` filter (defence in depth on top of schema isolation) and
//! the `is_deleted = false` soft-delete filter.

use chrono::{Datelike, Duration, NaiveDate, Utc, Weekday};
use kabipay_common::context::{ClientViewerEmployee, ScopeType};
use kabipay_common::workflow_approval;
use kabipay_common::workflow_current_step;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0010_time_shift_roster::{holiday, holiday_calendar};
use kabipay_db_entities::tenant::d0011_leave::{leave_balance, leave_request, leave_type};
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow, workflow_action, workflow_instance, workflow_step,
};
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Set, TransactionTrait,
};
use std::collections::HashSet;
use uuid::Uuid;

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

/// Leave requests the caller is allowed to see: `All` = tenant list; `Self` = own; `Team` = self +
/// direct reports; `Department` = same department; missing `viewer` for non-All → empty.
pub async fn list_requests(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
    from_date: Option<NaiveDate>,
    to_date: Option<NaiveDate>,
) -> KabiPayResult<Vec<leave_request::Model>> {
    if let (Some(from), Some(to)) = (from_date, to_date) {
        if from > to {
            return Err(KabiPayError::Validation(
                "fromDate must be on or before toDate".into(),
            ));
        }
    }
    let limit = limit.clamp(1, 200);
    let mut q = leave_request::Entity::find()
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::IsDeleted.eq(false));

    if let Some(from) = from_date {
        q = q.filter(leave_request::Column::ToDate.gte(from));
    }
    if let Some(to) = to_date {
        q = q.filter(leave_request::Column::FromDate.lte(to));
    }

    match scope {
        ScopeType::All => {}
        ScopeType::Self_ => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            q = q.filter(leave_request::Column::EmployeeId.eq(v.employee_id));
        }
        ScopeType::Team => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            let ids = team_member_employee_ids(db, tenant_id, v.employee_id).await?;
            if ids.is_empty() {
                return Ok(vec![]);
            }
            q = q.filter(leave_request::Column::EmployeeId.is_in(ids));
        }
        ScopeType::Department => {
            let Some(v) = viewer else {
                return Ok(vec![]);
            };
            let ids = department_peer_employee_ids(db, tenant_id, v).await?;
            if ids.is_empty() {
                return Ok(vec![]);
            }
            q = q.filter(leave_request::Column::EmployeeId.is_in(ids));
        }
    }

    q.order_by_desc(leave_request::Column::AppliedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Viewer plus everyone who reports to them.
async fn team_member_employee_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    manager_employee_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    let reports = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::ReportingManagerId.eq(manager_employee_id))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let mut ids: Vec<Uuid> = reports.into_iter().map(|e| e.id).collect();
    ids.push(manager_employee_id);
    Ok(ids)
}

/// Everyone in the same department, or only the caller when they have no department.
async fn department_peer_employee_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    viewer: ClientViewerEmployee,
) -> KabiPayResult<Vec<Uuid>> {
    let Some(d) = viewer.department_id else {
        return Ok(vec![viewer.employee_id]);
    };
    let rows = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::DepartmentId.eq(Some(d)))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(rows.into_iter().map(|e| e.id).collect())
}

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

/// Submit a leave request in one transaction: validate leave type, require a
/// provisioned balance row, check remaining `balance_days`, insert `leave_request`
/// with status `PENDING`, and increase `pending_days` on the balance.
pub async fn submit_leave_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
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

    let txn = db.begin().await?;

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

    let year = from_date.year();
    let bal = if lt.is_paid {
        let bal = leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(leave_type_id))
        .filter(leave_balance::Column::Year.eq(year))
        .one(&txn)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation(
                "no leave balance for this leave type and year — configure a leave policy with entitlement or ask HR to provision balances"
                    .into(),
            )
        })?;

        if bal.balance_days < days {
            return Err(KabiPayError::Validation(
                "insufficient leave balance for this request".into(),
            ));
        }
        Some(bal)
    } else {
        None
    };

    let req_id = Uuid::new_v4();
    let now = Utc::now();
    let am_req = leave_request::ActiveModel {
        id: Set(req_id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        leave_type_id: Set(leave_type_id),
        from_date: Set(from_date),
        to_date: Set(to_date),
        days_requested: Set(days),
        is_half_day: Set(is_half_day),
        half_day_session: Set(half_day_session),
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

    try_attach_leave_workflow(&txn, tenant_id, req_id, now).await?;

    if let Some(bal) = bal {
        let new_pending = bal.pending_days + days;
        let new_balance = bal.balance_days - days;
        let mut am_bal: leave_balance::ActiveModel = bal.into();
        am_bal.pending_days = Set(new_pending);
        am_bal.balance_days = Set(new_balance);
        am_bal.update(&txn).await?;
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

async fn load_leave_workflow_first_step(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
) -> KabiPayResult<Option<(workflow::Model, Uuid)>> {
    let wf = workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::IsActive.eq(true))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_LEAVE))
        .order_by_asc(workflow::Column::Name)
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let Some(wf) = wf else {
        return Ok(None);
    };
    let step = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(wf.id))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let Some(step) = step else {
        return Ok(None);
    };
    Ok(Some((wf, step.id)))
}

async fn try_attach_leave_workflow(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    leave_request_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let Some((wf, first_step_id)) = load_leave_workflow_first_step(txn, tenant_id).await? else {
        return Ok(());
    };
    let inst_id = Uuid::new_v4();
    let inst = workflow_instance::ActiveModel {
        id: Set(inst_id),
        tenant_id: Set(tenant_id),
        workflow_id: Set(wf.id),
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

async fn load_balance_for_request(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    model: &leave_request::Model,
) -> KabiPayResult<leave_balance::Model> {
    let year = model.from_date.year();
    leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(model.employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(model.leave_type_id))
        .filter(leave_balance::Column::Year.eq(year))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_balance",
            id: format!("{}-{}", model.employee_id, year),
        })
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

/// Final approval: leave row APPROVED, paid-leave balance movement when applicable, and outbox event.
async fn finalize_leave_approval(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    model: &leave_request::Model,
    bal: Option<&leave_balance::Model>,
    approver_user_id: Uuid,
    now: chrono::DateTime<Utc>,
    request_id: Uuid,
) -> KabiPayResult<()> {
    let days = model.days_requested;

    let mut am_req: leave_request::ActiveModel = model.clone().into();
    am_req.status = Set(STATUS_APPROVED.into());
    am_req.rejection_reason = Set(None);
    am_req.approved_by = Set(Some(approver_user_id));
    am_req.updated_at = Set(now);
    am_req.update(txn).await?;

    if let Some(bal) = bal {
        let consume_pending = bal.pending_days.min(days);
        if consume_pending != days {
            tracing::warn!(
                tenant_id = %tenant_id,
                employee_id = %model.employee_id,
                leave_request_id = %request_id,
                pending_days = %bal.pending_days,
                days_requested = %days,
                "leave balance pending_days below days_requested on approve; applying partial pending consumption"
            );
        }
        let new_pending = bal.pending_days - consume_pending;
        let new_used = bal.used_days + days;

        let mut am_bal: leave_balance::ActiveModel = bal.clone().into();
        am_bal.pending_days = Set(new_pending);
        am_bal.used_days = Set(new_used);
        am_bal.updated_at = Set(now);
        am_bal.update(txn).await?;
    }

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
    approver_user_id: Uuid,
) -> KabiPayResult<leave_request::Model> {
    let txn = db.begin().await?;
    let model = load_pending_request_in_txn(&txn, tenant_id, request_id).await?;
    let now = Utc::now();

    if let Some(inst_id) = model.workflow_instance_id {
        let mut inst = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| {
                KabiPayError::Validation("leave_request workflow_instance not found".into())
            })?;
        if inst.status != WF_STATUS_IN_PROGRESS {
            return Err(KabiPayError::Validation(
                "workflow instance is not in progress — cannot approve this leave".into(),
            ));
        }
        inst = workflow_current_step::ensure_workflow_instance_current_step_repaired(
            &txn, tenant_id, &inst, now,
        )
        .await?;
        let cur_step_id = inst.current_step_id.ok_or_else(|| {
            KabiPayError::Validation("workflow instance has no current step".into())
        })?;
        let cur_step = workflow_step::Entity::find_by_id(cur_step_id)
            .filter(workflow_step::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Validation("workflow step not found".into()))?;

        workflow_approval::assert_workflow_step_actor(
            &txn,
            tenant_id,
            approver_user_id,
            model.employee_id,
            &cur_step,
        )
        .await?;

        let act = workflow_action::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            instance_id: Set(inst_id),
            workflow_step_id: Set(cur_step_id),
            performed_by: Set(Some(approver_user_id)),
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
                .ok_or_else(|| {
                    KabiPayError::Internal("leave_request missing after commit".into())
                });
        }

        let mut am_inst: workflow_instance::ActiveModel = inst.into();
        am_inst.status = Set(WF_STATUS_COMPLETED.into());
        am_inst.current_step_id = Set(None);
        am_inst.completed_at = Set(Some(now));
        am_inst.updated_at = Set(now);
        am_inst.update(&txn).await?;

        let bal = if request_uses_leave_balance(&txn, tenant_id, &model).await? {
            Some(load_balance_for_request(&txn, tenant_id, &model).await?)
        } else {
            None
        };
        finalize_leave_approval(
            &txn,
            tenant_id,
            &model,
            bal.as_ref(),
            approver_user_id,
            now,
            request_id,
        )
        .await?;
    } else {
        if !workflow_approval::user_has_permission_via_roles(
            &txn,
            tenant_id,
            approver_user_id,
            "leave",
            "approve",
        )
        .await?
        {
            return Err(KabiPayError::Forbidden(
                "only users with leave approval permission may approve requests without a workflow"
                    .into(),
            ));
        }
        let bal = if request_uses_leave_balance(&txn, tenant_id, &model).await? {
            Some(load_balance_for_request(&txn, tenant_id, &model).await?)
        } else {
            None
        };
        finalize_leave_approval(
            &txn,
            tenant_id,
            &model,
            bal.as_ref(),
            approver_user_id,
            now,
            request_id,
        )
        .await?;
    }

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
    rejector_user_id: Uuid,
    rejection_reason: Option<String>,
) -> KabiPayResult<leave_request::Model> {
    let txn = db.begin().await?;
    let model = load_pending_request_in_txn(&txn, tenant_id, request_id).await?;

    if model.workflow_instance_id.is_none() {
        if !workflow_approval::user_has_permission_via_roles(
            &txn,
            tenant_id,
            rejector_user_id,
            "leave",
            "approve",
        )
        .await?
        {
            return Err(KabiPayError::Forbidden(
                "only users with leave approval permission may reject requests without a workflow"
                    .into(),
            ));
        }
    }

    if let Some(inst_id) = model.workflow_instance_id {
        if let Some(mut inst) = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
        {
            if inst.status == WF_STATUS_IN_PROGRESS {
                let now = Utc::now();
                inst = workflow_current_step::ensure_workflow_instance_current_step_repaired(
                    &txn, tenant_id, &inst, now,
                )
                .await?;
                if let Some(step_id) = inst.current_step_id {
                    let st = workflow_step::Entity::find_by_id(step_id)
                        .filter(workflow_step::Column::TenantId.eq(tenant_id))
                        .one(&txn)
                        .await?
                        .ok_or_else(|| KabiPayError::Validation("workflow step not found".into()))?;
                    workflow_approval::assert_workflow_step_actor(
                        &txn,
                        tenant_id,
                        rejector_user_id,
                        model.employee_id,
                        &st,
                    )
                    .await?;
                    let act = workflow_action::ActiveModel {
                        id: Set(Uuid::new_v4()),
                        tenant_id: Set(tenant_id),
                        instance_id: Set(inst_id),
                        workflow_step_id: Set(step_id),
                        performed_by: Set(Some(rejector_user_id)),
                        action: Set(WF_ACTION_REJECT.into()),
                        remarks: Set(rejection_reason.clone()),
                        acted_at: Set(now),
                        created_at: Set(now),
                        updated_at: Set(now),
                    };
                    act.insert(&txn).await?;
                }
                let mut am_inst: workflow_instance::ActiveModel = inst.into();
                am_inst.status = Set(WF_STATUS_REJECTED.into());
                am_inst.completed_at = Set(Some(now));
                am_inst.updated_at = Set(now);
                am_inst.update(&txn).await?;
            }
        }
    }

    let now = Utc::now();
    if request_uses_leave_balance(&txn, tenant_id, &model).await? {
        let year = model.from_date.year();
        let days = model.days_requested;
        let bal = leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(model.employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(model.leave_type_id))
        .filter(leave_balance::Column::Year.eq(year))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_balance",
            id: format!("{}-{}", model.employee_id, year),
        })?;

    let release_pending = bal.pending_days.min(days);
    if release_pending != days {
        tracing::warn!(
            tenant_id = %tenant_id,
            employee_id = %model.employee_id,
            leave_request_id = %request_id,
            pending_days = %bal.pending_days,
            days_requested = %days,
            "leave balance pending_days below days_requested on reject; restoring balance_days fully"
        );
    }
    let new_pending = bal.pending_days - release_pending;
    let new_balance = bal.balance_days + days;

        let mut am_bal: leave_balance::ActiveModel = bal.into();
        am_bal.pending_days = Set(new_pending);
        am_bal.balance_days = Set(new_balance);
        am_bal.updated_at = Set(now);
        am_bal.update(&txn).await?;
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
    canceller_employee_id: Uuid,
) -> KabiPayResult<leave_request::Model> {
    let txn = db.begin().await?;
    let model = load_pending_request_in_txn(&txn, tenant_id, request_id).await?;

    if model.employee_id != canceller_employee_id {
        return Err(KabiPayError::Forbidden(
            "only the employee who submitted the request may cancel a pending request".into(),
        ));
    }

    let now = Utc::now();

    if let Some(inst_id) = model.workflow_instance_id {
        if let Some(inst) = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
        {
            if inst.status == WF_STATUS_IN_PROGRESS {
                let mut am_inst: workflow_instance::ActiveModel = inst.into();
                am_inst.status = Set(WF_STATUS_CANCELLED.into());
                am_inst.completed_at = Set(Some(now));
                am_inst.updated_at = Set(now);
                am_inst.update(&txn).await?;
            }
        }
    }

    if request_uses_leave_balance(&txn, tenant_id, &model).await? {
        let year = model.from_date.year();
        let days = model.days_requested;
        let bal = leave_balance::Entity::find()
        .filter(leave_balance::Column::TenantId.eq(tenant_id))
        .filter(leave_balance::Column::EmployeeId.eq(model.employee_id))
        .filter(leave_balance::Column::LeaveTypeId.eq(model.leave_type_id))
        .filter(leave_balance::Column::Year.eq(year))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_balance",
            id: format!("{}-{}", model.employee_id, year),
        })?;

    let release_pending = bal.pending_days.min(days);
    if release_pending != days {
        tracing::warn!(
            tenant_id = %tenant_id,
            employee_id = %model.employee_id,
            leave_request_id = %request_id,
            pending_days = %bal.pending_days,
            days_requested = %days,
            "leave balance pending_days below days_requested on cancel; restoring balance_days fully"
        );
    }
    let new_pending = bal.pending_days - release_pending;
    let new_balance = bal.balance_days + days;

        let mut am_bal: leave_balance::ActiveModel = bal.into();
        am_bal.pending_days = Set(new_pending);
        am_bal.balance_days = Set(new_balance);
        am_bal.updated_at = Set(now);
        am_bal.update(&txn).await?;
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

async fn load_pending_request_in_txn(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    request_id: Uuid,
) -> KabiPayResult<leave_request::Model> {
    let m = leave_request::Entity::find_by_id(request_id)
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "leave_request",
            id: request_id.to_string(),
        })?;
    if m.status != STATUS_PENDING {
        return Err(KabiPayError::Validation(
            "only PENDING leave requests can be approved or rejected".into(),
        ));
    }
    Ok(m)
}

/// Whether `subject_employee_id`'s leave rows may be read given JWT scope + viewer.
pub async fn employee_may_view_leave_subject(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    subject_employee_id: Uuid,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<bool> {
    match scope {
        ScopeType::All => Ok(true),
        ScopeType::Self_ => Ok(viewer
            .map(|v| v.employee_id == subject_employee_id)
            .unwrap_or(false)),
        ScopeType::Team => {
            let Some(v) = viewer else {
                return Ok(false);
            };
            let ids = team_member_employee_ids(db, tenant_id, v.employee_id).await?;
            Ok(ids.contains(&subject_employee_id))
        }
        ScopeType::Department => {
            let Some(v) = viewer else {
                return Ok(false);
            };
            let ids = department_peer_employee_ids(db, tenant_id, v).await?;
            Ok(ids.contains(&subject_employee_id))
        }
    }
}

pub async fn load_leave_request_for_viewer(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    scope: ScopeType,
    viewer: Option<ClientViewerEmployee>,
) -> KabiPayResult<Option<leave_request::Model>> {
    let m = leave_request::Entity::find_by_id(request_id)
        .filter(leave_request::Column::TenantId.eq(tenant_id))
        .filter(leave_request::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let Some(row) = m else {
        return Ok(None);
    };
    let ok = employee_may_view_leave_subject(db, tenant_id, row.employee_id, scope, viewer).await?;
    if !ok {
        return Ok(None);
    }
    Ok(Some(row))
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
