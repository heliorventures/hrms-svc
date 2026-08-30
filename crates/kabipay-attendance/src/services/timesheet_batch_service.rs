//! Weekly timesheet submission (`timesheet_week_batch`) + leave-style workflow approvals.

use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{
    client_data_scope::EmployeeScopeFilter,
    context::{is_active_employment_status, ClientViewerEmployee},
    workflow_approval::{self, WorkflowApprovalAuthority},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0010_time_shift_roster::{timesheet_entry, timesheet_week_batch};
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow, workflow_action, workflow_instance, workflow_step,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

use crate::services::{timesheet_dates::week_monday_sunday, timesheet_policy};

/// Matches `workflow.entity_type` / seed for timesheet week batches.
pub const WF_ENTITY_TIMESHEET_WEEK_BATCH: &str = "TIMESHEET_WEEK_BATCH";

const BATCH_PENDING: &str = "PENDING";
const BATCH_APPROVED: &str = "APPROVED";
const BATCH_REJECTED: &str = "REJECTED";

const ENTRY_DRAFT: &str = "DRAFT";
const ENTRY_SUBMITTED: &str = "SUBMITTED";
const ENTRY_APPROVED: &str = "APPROVED";

const WF_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
const WF_STATUS_COMPLETED: &str = "COMPLETED";
const WF_STATUS_CANCELLED: &str = "CANCELLED";
const WF_ACTION_APPROVE: &str = "APPROVE";
const WF_ACTION_REJECT: &str = "REJECT";

fn assert_monday(week_start: NaiveDate) -> KabiPayResult<()> {
    if week_start.weekday() != chrono::Weekday::Mon {
        return Err(KabiPayError::Validation(
            "weekStartDate must be a Monday".into(),
        ));
    }
    Ok(())
}

async fn load_timesheet_workflow_first_step(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
) -> KabiPayResult<Option<(workflow::Model, Uuid)>> {
    let wf = workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::IsActive.eq(true))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_TIMESHEET_WEEK_BATCH))
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

async fn try_attach_timesheet_workflow(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    batch_id: Uuid,
    subject_employee_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let Some((wf, first_step_id)) = load_timesheet_workflow_first_step(txn, tenant_id).await? else {
        return Ok(());
    };
    let inst_id = Uuid::new_v4();
    let wf_id = wf.id;
    let inst = workflow_instance::ActiveModel {
        id: Set(inst_id),
        tenant_id: Set(tenant_id),
        workflow_id: Set(wf_id),
        entity_type: Set(WF_ENTITY_TIMESHEET_WEEK_BATCH.into()),
        entity_id: Set(batch_id),
        status: Set(WF_STATUS_IN_PROGRESS.into()),
        current_step_id: Set(Some(first_step_id)),
        created_at: Set(now),
        completed_at: Set(None),
        updated_at: Set(now),
    };
    inst.insert(txn).await.map_err(KabiPayError::from)?;

    let mut am_batch: timesheet_week_batch::ActiveModel =
        timesheet_week_batch::Entity::find_by_id(batch_id)
            .one(txn)
            .await?
            .ok_or_else(|| KabiPayError::Internal("timesheet_week_batch missing after insert".into()))?
            .into();
    am_batch.workflow_instance_id = Set(Some(inst_id));
    am_batch.updated_at = Set(now);
    am_batch.update(txn).await.map_err(KabiPayError::from)?;
    let _ = subject_employee_id;
    Ok(())
}

fn timesheet_decision_not_actionable() -> KabiPayError {
    KabiPayError::Validation(
        "this timesheet approval is no longer actionable; refresh and try again".into(),
    )
}

fn pending_batch_for_decision_query(
    batch_id: Uuid,
    tenant_id: Uuid,
) -> sea_orm::Select<timesheet_week_batch::Entity> {
    timesheet_week_batch::Entity::find_by_id(batch_id)
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
        .filter(timesheet_week_batch::Column::Status.eq(BATCH_PENDING))
        .lock_exclusive()
}

fn workflow_instance_for_decision_query(
    instance_id: Uuid,
    tenant_id: Uuid,
    batch_id: Uuid,
) -> sea_orm::Select<workflow_instance::Entity> {
    workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq(WF_ENTITY_TIMESHEET_WEEK_BATCH))
        .filter(workflow_instance::Column::EntityId.eq(batch_id))
        .filter(workflow_instance::Column::Status.eq(WF_STATUS_IN_PROGRESS))
        .lock_exclusive()
}

fn workflow_for_decision_query(
    workflow_id: Uuid,
    tenant_id: Uuid,
) -> sea_orm::Select<workflow::Entity> {
    workflow::Entity::find_by_id(workflow_id)
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_TIMESHEET_WEEK_BATCH))
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

fn employee_rows_for_update_query(
    tenant_id: Uuid,
    actor_employee_id: Uuid,
    subject_employee_id: Uuid,
) -> sea_orm::Select<employee::Entity> {
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.is_in([actor_employee_id, subject_employee_id]))
        .order_by_asc(employee::Column::Id)
        .lock_exclusive()
}

fn require_workflow_instance_id(instance_id: Option<Uuid>) -> KabiPayResult<Uuid> {
    instance_id.ok_or_else(timesheet_decision_not_actionable)
}

fn require_expected_workflow_step(
    current_step_id: Option<Uuid>,
    expected_workflow_step_id: Uuid,
) -> KabiPayResult<()> {
    if current_step_id == Some(expected_workflow_step_id) {
        Ok(())
    } else {
        Err(timesheet_decision_not_actionable())
    }
}

fn validate_locked_decision_employees(
    rows: Vec<employee::Model>,
    tenant_id: Uuid,
    actor_employee_id: Uuid,
    subject_employee_id: Uuid,
    actor_user_id: Uuid,
) -> KabiPayResult<(ClientViewerEmployee, employee::Model)> {
    if actor_employee_id == subject_employee_id {
        return Err(KabiPayError::Forbidden(
            "you cannot approve or reject your own timesheet submission".into(),
        ));
    }

    let actor = rows
        .iter()
        .find(|row| row.id == actor_employee_id)
        .cloned()
        .ok_or_else(timesheet_decision_not_actionable)?;
    let subject = rows
        .into_iter()
        .find(|row| row.id == subject_employee_id)
        .ok_or_else(timesheet_decision_not_actionable)?;

    let actor_is_valid = actor.tenant_id == tenant_id
        && !actor.is_deleted
        && is_active_employment_status(&actor.status)
        && actor.user_id == Some(actor_user_id);
    let subject_is_valid = subject.tenant_id == tenant_id
        && !subject.is_deleted
        && is_active_employment_status(&subject.status);
    if !actor_is_valid || !subject_is_valid {
        return Err(timesheet_decision_not_actionable());
    }

    Ok((
        ClientViewerEmployee {
            employee_id: actor.id,
            department_id: actor.department_id,
        },
        subject,
    ))
}

async fn lock_and_validate_decision_employees(
    txn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    subject_employee_id: Uuid,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<(ClientViewerEmployee, employee::Model)> {
    let actor_employee_id = authority
        .actor_employee
        .map(|actor| actor.employee_id)
        .ok_or_else(timesheet_decision_not_actionable)?;
    let rows = employee_rows_for_update_query(
        tenant_id,
        actor_employee_id,
        subject_employee_id,
    )
    .all(txn)
    .await?;
    validate_locked_decision_employees(
        rows,
        tenant_id,
        actor_employee_id,
        subject_employee_id,
        authority.actor_user_id,
    )
}

async fn lock_current_timesheet_workflow(
    txn: &(impl ConnectionTrait + Sync),
    tenant_id: Uuid,
    batch: &timesheet_week_batch::Model,
    expected_workflow_step_id: Uuid,
) -> KabiPayResult<(workflow_instance::Model, workflow_step::Model)> {
    let instance_id = require_workflow_instance_id(batch.workflow_instance_id)?;
    let instance = workflow_instance_for_decision_query(instance_id, tenant_id, batch.id)
        .one(txn)
        .await?
        .ok_or_else(timesheet_decision_not_actionable)?;
    require_expected_workflow_step(instance.current_step_id, expected_workflow_step_id)?;
    workflow_for_decision_query(instance.workflow_id, tenant_id)
        .one(txn)
        .await?
        .ok_or_else(timesheet_decision_not_actionable)?;
    let step = workflow_step_for_decision_query(
        expected_workflow_step_id,
        tenant_id,
        instance.workflow_id,
    )
    .one(txn)
    .await?
    .ok_or_else(timesheet_decision_not_actionable)?;
    Ok((instance, step))
}

/// Employee submits all draft rows Mon-Sun `week_start`; starts workflow when configured.
pub async fn submit_timesheet_week(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    week_start: NaiveDate,
) -> KabiPayResult<timesheet_week_batch::Model> {
    assert_monday(week_start)?;
    let (mon, sun) = week_monday_sunday(week_start);

    timesheet_policy::assert_work_date_allowed_for_entry(db, tenant_id, sun).await?;
    timesheet_policy::assert_week_hours_for_submission(db, tenant_id, employee_id, mon).await?;

    let dup = timesheet_week_batch::Entity::find()
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
        .filter(timesheet_week_batch::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_week_batch::Column::WeekStartDate.eq(mon))
        .filter(timesheet_week_batch::Column::Status.is_in(vec![
            BATCH_PENDING.to_string(),
            BATCH_APPROVED.to_string(),
        ]))
        .one(db)
        .await?;
    if dup.is_some() {
        return Err(KabiPayError::Validation(
            "this week already has a submission".into(),
        ));
    }

    let drafts = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false))
        .filter(timesheet_entry::Column::WorkDate.gte(mon))
        .filter(timesheet_entry::Column::WorkDate.lte(sun))
        .filter(timesheet_entry::Column::Status.eq(ENTRY_DRAFT))
        .filter(timesheet_entry::Column::BatchId.is_null())
        .all(db)
        .await?;

    if drafts.is_empty() {
        return Err(KabiPayError::Validation(
            "no draft timesheet rows in this week to submit".into(),
        ));
    }

    let txn = db.begin().await?;
    let now = Utc::now();
    let existing_batch = timesheet_week_batch::Entity::find()
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
        .filter(timesheet_week_batch::Column::EmployeeId.eq(employee_id))
        .filter(timesheet_week_batch::Column::WeekStartDate.eq(mon))
        .lock_exclusive()
        .one(&txn)
        .await?;

    let batch_id = if let Some(batch) = existing_batch {
        let status = batch.status.trim().to_uppercase();
        if status == BATCH_PENDING || status == BATCH_APPROVED {
            return Err(KabiPayError::Validation(
                "this week already has a submission".into(),
            ));
        }
        if status != BATCH_REJECTED {
            return Err(KabiPayError::Validation(format!(
                "timesheet week cannot be resubmitted from status {}",
                batch.status
            )));
        }
        let batch_id = batch.id;
        let mut batch_am: timesheet_week_batch::ActiveModel = batch.into();
        batch_am.status = Set(BATCH_PENDING.into());
        batch_am.workflow_instance_id = Set(None);
        batch_am.submitted_at = Set(Some(now));
        batch_am.rejection_reason = Set(None);
        batch_am.updated_at = Set(now);
        batch_am.update(&txn).await?;
        batch_id
    } else {
        let batch_id = Uuid::new_v4();
        let batch_am = timesheet_week_batch::ActiveModel {
            id: Set(batch_id),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            week_start_date: Set(mon),
            status: Set(BATCH_PENDING.into()),
            workflow_instance_id: Set(None),
            submitted_at: Set(Some(now)),
            rejection_reason: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        };
        batch_am.insert(&txn).await?;
        batch_id
    };

    try_attach_timesheet_workflow(&txn, tenant_id, batch_id, employee_id, now).await?;

    for row in &drafts {
        let mut am: timesheet_entry::ActiveModel = row.clone().into();
        am.batch_id = Set(Some(batch_id));
        am.status = Set(ENTRY_SUBMITTED.into());
        am.updated_at = Set(now);
        am.update(&txn).await?;
    }

    txn.commit().await?;

    timesheet_week_batch::Entity::find_by_id(batch_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("timesheet_week_batch not found after submit".into()))
}

async fn finalize_batch_approved(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    batch_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let mut am_batch: timesheet_week_batch::ActiveModel =
        timesheet_week_batch::Entity::find_by_id(batch_id)
            .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
            .one(txn)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "timesheet_week_batch",
                id: batch_id.to_string(),
            })?
            .into();
    am_batch.status = Set(BATCH_APPROVED.into());
    am_batch.updated_at = Set(now);
    am_batch.update(txn).await?;

    let rows = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::BatchId.eq(batch_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false))
        .all(txn)
        .await?;

    for row in rows {
        let mut am: timesheet_entry::ActiveModel = row.into();
        am.status = Set(ENTRY_APPROVED.into());
        am.updated_at = Set(now);
        am.update(txn).await?;
    }
    Ok(())
}

pub async fn approve_timesheet_week_batch(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    batch_id: Uuid,
    expected_workflow_step_id: Uuid,
    authority: &WorkflowApprovalAuthority,
) -> KabiPayResult<timesheet_week_batch::Model> {
    let txn = db.begin().await?;
    let batch = pending_batch_for_decision_query(batch_id, tenant_id)
        .one(&txn)
        .await?
        .ok_or_else(timesheet_decision_not_actionable)?;
    let (actor, subject) = lock_and_validate_decision_employees(
        &txn,
        tenant_id,
        batch.employee_id,
        authority,
    )
    .await?;
    if subject.id != batch.employee_id {
        return Err(timesheet_decision_not_actionable());
    }
    let validated_authority = WorkflowApprovalAuthority {
        actor_user_id: authority.actor_user_id,
        actor_employee: Some(actor),
        scope: authority.scope,
        permission: authority.permission,
    };
    workflow_approval::assert_subject_in_approval_scope(
        &txn,
        tenant_id,
        batch.employee_id,
        &validated_authority,
    )
    .await?;
    let approver_user_id = validated_authority.actor_user_id;
    let now = Utc::now();
    let (instance, current_step) = lock_current_timesheet_workflow(
        &txn,
        tenant_id,
        &batch,
        expected_workflow_step_id,
    )
    .await?;
    workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
        &txn,
        tenant_id,
        batch.employee_id,
        &current_step,
        &validated_authority,
    )
    .await?;

    workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        instance_id: Set(instance.id),
        workflow_step_id: Set(current_step.id),
        performed_by: Set(Some(approver_user_id)),
        action: Set(WF_ACTION_APPROVE.into()),
        remarks: Set(None),
        acted_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;

    let next_step = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(instance.workflow_id))
        .filter(workflow_step::Column::SequenceOrder.gt(current_step.sequence_order))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .one(&txn)
        .await?;

    let mut active_instance: workflow_instance::ActiveModel = instance.into();
    if let Some(next) = next_step {
        active_instance.current_step_id = Set(Some(next.id));
        active_instance.updated_at = Set(now);
        active_instance.update(&txn).await?;
    } else {
        active_instance.status = Set(WF_STATUS_COMPLETED.into());
        active_instance.current_step_id = Set(None);
        active_instance.completed_at = Set(Some(now));
        active_instance.updated_at = Set(now);
        active_instance.update(&txn).await?;
        finalize_batch_approved(&txn, tenant_id, batch_id, now).await?;
    }
    txn.commit().await?;

    timesheet_week_batch::Entity::find_by_id(batch_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("batch missing".into()))
}

pub async fn reject_timesheet_week_batch(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    batch_id: Uuid,
    expected_workflow_step_id: Uuid,
    authority: &WorkflowApprovalAuthority,
    rejection_reason: Option<String>,
) -> KabiPayResult<bool> {
    let txn = db.begin().await?;
    let batch = pending_batch_for_decision_query(batch_id, tenant_id)
        .one(&txn)
        .await?
        .ok_or_else(timesheet_decision_not_actionable)?;
    let (actor, subject) = lock_and_validate_decision_employees(
        &txn,
        tenant_id,
        batch.employee_id,
        authority,
    )
    .await?;
    if subject.id != batch.employee_id {
        return Err(timesheet_decision_not_actionable());
    }
    let validated_authority = WorkflowApprovalAuthority {
        actor_user_id: authority.actor_user_id,
        actor_employee: Some(actor),
        scope: authority.scope,
        permission: authority.permission,
    };
    workflow_approval::assert_subject_in_approval_scope(
        &txn,
        tenant_id,
        batch.employee_id,
        &validated_authority,
    )
    .await?;
    let rejector_user_id = validated_authority.actor_user_id;
    let now = Utc::now();
    let sanitized_reason = rejection_reason.and_then(|reason| {
        let trimmed = reason.trim().to_string();
        (!trimmed.is_empty()).then_some(trimmed)
    });
    let (instance, current_step) = lock_current_timesheet_workflow(
        &txn,
        tenant_id,
        &batch,
        expected_workflow_step_id,
    )
    .await?;
    workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
        &txn,
        tenant_id,
        batch.employee_id,
        &current_step,
        &validated_authority,
    )
    .await?;
    workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        instance_id: Set(instance.id),
        workflow_step_id: Set(current_step.id),
        performed_by: Set(Some(rejector_user_id)),
        action: Set(WF_ACTION_REJECT.into()),
        remarks: Set(sanitized_reason.clone()),
        acted_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;
    let mut active_instance: workflow_instance::ActiveModel = instance.into();
    active_instance.status = Set(WF_STATUS_CANCELLED.into());
    active_instance.current_step_id = Set(None);
    active_instance.completed_at = Set(Some(now));
    active_instance.updated_at = Set(now);
    active_instance.update(&txn).await?;

    let rows = timesheet_entry::Entity::find()
        .filter(timesheet_entry::Column::TenantId.eq(tenant_id))
        .filter(timesheet_entry::Column::BatchId.eq(batch_id))
        .filter(timesheet_entry::Column::IsDeleted.eq(false))
        .all(&txn)
        .await?;

    for row in rows {
        let mut am: timesheet_entry::ActiveModel = row.into();
        am.batch_id = Set(None);
        am.status = Set(ENTRY_DRAFT.into());
        am.updated_at = Set(now);
        am.update(&txn).await?;
    }

    let rejected_employee_id = batch.employee_id;
    let rejected_week_start = batch.week_start_date;

    let mut am_batch: timesheet_week_batch::ActiveModel = batch.into();
    am_batch.status = Set(BATCH_REJECTED.into());
    am_batch.rejection_reason = Set(sanitized_reason.clone());
    am_batch.updated_at = Set(now);
    am_batch.update(&txn).await?;

    txn.commit().await?;

    crate::services::timesheet_notification_service::notify_employee_timesheet_rejected(
        db,
        tenant_id,
        rejected_employee_id,
        rejected_week_start,
        sanitized_reason.as_deref(),
        now,
    )
    .await?;
    Ok(true)
}

pub async fn list_timesheet_week_batches(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: Option<String>,
    limit: u64,
    scope_filter: &EmployeeScopeFilter,
) -> KabiPayResult<Vec<timesheet_week_batch::Model>> {
    let limit = limit.clamp(1, 200);
    match scope_filter {
        EmployeeScopeFilter::Empty => return Ok(vec![]),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(vec![]),
        _ => {}
    }

    let mut q = timesheet_week_batch::Entity::find()
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id));

    if let EmployeeScopeFilter::EmployeeIds(ids) = scope_filter {
        q = q.filter(timesheet_week_batch::Column::EmployeeId.is_in(ids.clone()));
    }

    if let Some(st) = status {
        let u = st.trim().to_uppercase();
        if !u.is_empty() {
            q = q.filter(timesheet_week_batch::Column::Status.eq(u));
        }
    }

    q.order_by_desc(timesheet_week_batch::Column::SubmittedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub const BATCH_PENDING_STATUS: &str = "PENDING";

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct TimesheetApprovalSnapshot {
    pub pending_stage: Option<String>,
    pub actionable_step_id: Option<Uuid>,
}

/// Resolves the display stage and current actor-specific action token from one workflow lookup.
pub async fn resolve_timesheet_approval_snapshot(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    batch_id: Uuid,
    status: &str,
    subject_employee_id: Uuid,
    workflow_instance_id: Option<Uuid>,
    authority: Option<&WorkflowApprovalAuthority>,
) -> KabiPayResult<TimesheetApprovalSnapshot> {
    if !status
        .trim()
        .eq_ignore_ascii_case(BATCH_PENDING_STATUS)
    {
        return Ok(TimesheetApprovalSnapshot::default());
    }
    let Some(instance_id) = workflow_instance_id else {
        return Ok(TimesheetApprovalSnapshot::default());
    };
    let Some(instance) = actionable_timesheet_workflow_instance_query(
        instance_id,
        tenant_id,
        batch_id,
    )
    .one(db)
    .await?
    else {
        return Ok(TimesheetApprovalSnapshot::default());
    };
    let Some(_workflow) =
        actionable_timesheet_workflow_query(instance.workflow_id, tenant_id)
            .one(db)
            .await?
    else {
        return Ok(TimesheetApprovalSnapshot::default());
    };
    let Some(current_step_id) = instance.current_step_id else {
        return Ok(TimesheetApprovalSnapshot::default());
    };
    let Some(step) = actionable_timesheet_workflow_step_query(
        current_step_id,
        tenant_id,
        instance.workflow_id,
    )
    .one(db)
    .await?
    else {
        return Ok(TimesheetApprovalSnapshot::default());
    };

    let mut snapshot = TimesheetApprovalSnapshot {
        pending_stage: Some(step.step_name.clone()),
        actionable_step_id: None,
    };
    let Some(authority) = authority else {
        return Ok(snapshot);
    };
    let Some(actor) = authority.actor_employee else {
        return Ok(snapshot);
    };
    if actor.employee_id == subject_employee_id {
        return Ok(snapshot);
    }

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
        return Ok(snapshot);
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
        return Ok(snapshot);
    }

    if workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
        db,
        tenant_id,
        subject_employee_id,
        &step,
        authority,
    )
    .await
    .is_ok()
    {
        snapshot.actionable_step_id = Some(step.id);
    }
    Ok(snapshot)
}

fn actionable_timesheet_workflow_instance_query(
    instance_id: Uuid,
    tenant_id: Uuid,
    batch_id: Uuid,
) -> sea_orm::Select<workflow_instance::Entity> {
    workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .filter(workflow_instance::Column::EntityType.eq(WF_ENTITY_TIMESHEET_WEEK_BATCH))
        .filter(workflow_instance::Column::EntityId.eq(batch_id))
        .filter(workflow_instance::Column::Status.eq(WF_STATUS_IN_PROGRESS))
}

fn actionable_timesheet_workflow_query(
    workflow_id: Uuid,
    tenant_id: Uuid,
) -> sea_orm::Select<workflow::Entity> {
    workflow::Entity::find_by_id(workflow_id)
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_TIMESHEET_WEEK_BATCH))
}

fn actionable_timesheet_workflow_step_query(
    step_id: Uuid,
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> sea_orm::Select<workflow_step::Entity> {
    workflow_step::Entity::find_by_id(step_id)
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
}

#[cfg(test)]
mod decision_contract_tests {
    use super::*;
    use sea_orm::{DbBackend, QueryTrait};

    fn employee_row(
        id: Uuid,
        tenant_id: Uuid,
        user_id: Option<Uuid>,
        status: &str,
    ) -> employee::Model {
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
            employment_type: None,
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
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn decision_queries_lock_batch_instance_workflow_and_expected_step() {
        let tenant_id = Uuid::new_v4();
        let batch_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let step_id = Uuid::new_v4();

        let batch_sql = pending_batch_for_decision_query(batch_id, tenant_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(batch_sql.contains("\"status\" = 'PENDING'"));
        assert!(batch_sql.ends_with("FOR UPDATE"));

        let instance_sql = workflow_instance_for_decision_query(
            instance_id,
            tenant_id,
            batch_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(instance_sql.contains("\"entity_type\" = 'TIMESHEET_WEEK_BATCH'"));
        assert!(instance_sql.contains(&batch_id.to_string()));
        assert!(instance_sql.contains("\"status\" = 'IN_PROGRESS'"));
        assert!(instance_sql.ends_with("FOR UPDATE"));

        let workflow_sql = workflow_for_decision_query(workflow_id, tenant_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(workflow_sql.contains("\"entity_type\" = 'TIMESHEET_WEEK_BATCH'"));
        assert!(workflow_sql.ends_with("FOR UPDATE"));

        let step_sql = workflow_step_for_decision_query(step_id, tenant_id, workflow_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(step_sql.contains(&workflow_id.to_string()));
        assert!(step_sql.ends_with("FOR UPDATE"));

        let actor_id = Uuid::new_v4();
        let subject_id = Uuid::new_v4();
        let employee_sql = employee_rows_for_update_query(
            tenant_id,
            actor_id,
            subject_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(employee_sql.contains("ORDER BY \"employee\".\"id\" ASC"));
        assert!(employee_sql.ends_with("FOR UPDATE"));
    }

    #[test]
    fn stale_duplicate_and_workflowless_decisions_fail_closed() {
        let expected_step_id = Uuid::new_v4();
        assert!(require_expected_workflow_step(Some(expected_step_id), expected_step_id).is_ok());

        for current in [None, Some(Uuid::new_v4())] {
            let error = require_expected_workflow_step(current, expected_step_id)
                .expect_err("workflowless or stale decisions must fail");
            assert_eq!(error.code(), "VALIDATION_ERROR");
        }

        let error = require_workflow_instance_id(None)
            .expect_err("workflowless timesheet batches must not be directly decided");
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn locked_actor_and_subject_must_be_active_linked_and_distinct() {
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
        )
        .expect("active actor and subject are valid");
        assert_eq!(actor.employee_id, actor_id);
        assert_eq!(actor.department_id, None);
        assert_eq!(subject.id, subject_id);

        let mut inactive_actor = rows.clone();
        inactive_actor
            .iter_mut()
            .find(|row| row.id == actor_id)
            .expect("actor")
            .status = "INACTIVE".into();
        assert!(validate_locked_decision_employees(
            inactive_actor,
            tenant_id,
            actor_id,
            subject_id,
            actor_user_id,
        )
        .is_err());

        let mut deleted_subject = rows;
        deleted_subject
            .iter_mut()
            .find(|row| row.id == subject_id)
            .expect("subject")
            .is_deleted = true;
        assert!(validate_locked_decision_employees(
            deleted_subject,
            tenant_id,
            actor_id,
            subject_id,
            actor_user_id,
        )
        .is_err());

        let self_row = vec![employee_row(
            actor_id,
            tenant_id,
            Some(actor_user_id),
            "ACTIVE",
        )];
        assert!(validate_locked_decision_employees(
            self_row,
            tenant_id,
            actor_id,
            actor_id,
            actor_user_id,
        )
        .is_err());
    }

    #[tokio::test]
    async fn approval_snapshot_fails_closed_before_database_access_when_not_actionable() {
        let actor_id = Uuid::new_v4();
        let authority = WorkflowApprovalAuthority {
            actor_user_id: Uuid::new_v4(),
            actor_employee: Some(ClientViewerEmployee {
                employee_id: actor_id,
                department_id: None,
            }),
            scope: kabipay_common::context::ScopeType::All,
            permission: kabipay_common::context::PERM_TIMESHEET_APPROVE,
        };

        for (status, subject_id, instance_id) in [
            ("APPROVED", Uuid::new_v4(), Some(Uuid::new_v4())),
            ("PENDING", Uuid::new_v4(), None),
        ] {
            let snapshot = resolve_timesheet_approval_snapshot(
                &DatabaseConnection::Disconnected,
                Uuid::new_v4(),
                Uuid::new_v4(),
                status,
                subject_id,
                instance_id,
                Some(&authority),
            )
            .await
            .expect("non-actionable rows resolve to an empty snapshot");
            assert_eq!(snapshot, TimesheetApprovalSnapshot::default());
        }
    }

    #[test]
    fn approval_snapshot_queries_bind_instance_workflow_and_step_to_timesheet_batch() {
        let tenant_id = Uuid::new_v4();
        let batch_id = Uuid::new_v4();
        let instance_id = Uuid::new_v4();
        let workflow_id = Uuid::new_v4();
        let step_id = Uuid::new_v4();

        let instance_sql = actionable_timesheet_workflow_instance_query(
            instance_id,
            tenant_id,
            batch_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(instance_sql.contains(&batch_id.to_string()));
        assert!(instance_sql.contains("\"entity_type\" = 'TIMESHEET_WEEK_BATCH'"));
        assert!(instance_sql.contains("\"status\" = 'IN_PROGRESS'"));

        let workflow_sql = actionable_timesheet_workflow_query(workflow_id, tenant_id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(workflow_sql.contains("\"entity_type\" = 'TIMESHEET_WEEK_BATCH'"));

        let step_sql = actionable_timesheet_workflow_step_query(
            step_id,
            tenant_id,
            workflow_id,
        )
        .build(DbBackend::Postgres)
        .to_string();
        assert!(step_sql.contains(&workflow_id.to_string()));
    }
}
