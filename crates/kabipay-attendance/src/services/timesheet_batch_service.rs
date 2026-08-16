//! Weekly timesheet submission (`timesheet_week_batch`) + leave-style workflow approvals.

use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{
    client_data_scope::EmployeeScopeFilter,
    workflow_approval,
    workflow_current_step,
    workflow_inbox::{
        self,
        NoWorkflowInstanceGate,
        WorkflowActorCheckMode,
    },
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

async fn assert_actor_is_not_subject_employee(
    conn: &impl ConnectionTrait,
    tenant_id: Uuid,
    actor_user_id: Uuid,
    subject_employee_id: Uuid,
) -> KabiPayResult<()> {
    let subject = employee::Entity::find_by_id(subject_employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(conn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employee",
            id: subject_employee_id.to_string(),
        })?;

    if subject.user_id == Some(actor_user_id) {
        return Err(KabiPayError::Forbidden(
            "you cannot approve or reject your own timesheet submission".into(),
        ));
    }

    Ok(())
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

async fn approve_without_workflow_permission_check(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    batch_id: Uuid,
    approver_user_id: Uuid,
    subject_employee_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    if !workflow_approval::user_has_permission_via_roles(
        txn,
        tenant_id,
        approver_user_id,
        "timesheet",
        "approve",
    )
    .await?
    {
        return Err(KabiPayError::Forbidden(
            "timesheet approval requires timesheet:approve (or tenant HR role)".into(),
        ));
    }
    let _ = subject_employee_id;
    finalize_batch_approved(txn, tenant_id, batch_id, now).await?;
    Ok(())
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
    approver_user_id: Uuid,
) -> KabiPayResult<timesheet_week_batch::Model> {
    let txn = db.begin().await?;
    let batch = timesheet_week_batch::Entity::find_by_id(batch_id)
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "timesheet_week_batch",
            id: batch_id.to_string(),
        })?;

    if batch.status != BATCH_PENDING {
        return Err(KabiPayError::Validation(
            "only pending submissions can be approved".into(),
        ));
    }
    assert_actor_is_not_subject_employee(&txn, tenant_id, approver_user_id, batch.employee_id)
        .await?;

    let now = Utc::now();

    if let Some(inst_id) = batch.workflow_instance_id {
        let mut inst = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Validation("workflow instance not found".into()))?;

        if inst.status != WF_STATUS_IN_PROGRESS {
            return Err(KabiPayError::Validation(
                "workflow instance is not active".into(),
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

        workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
            &txn,
            tenant_id,
            approver_user_id,
            batch.employee_id,
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
            return timesheet_week_batch::Entity::find_by_id(batch_id)
                .one(db)
                .await?
                .ok_or_else(|| KabiPayError::Internal("batch missing".into()));
        }

        let mut am_inst: workflow_instance::ActiveModel = inst.into();
        am_inst.status = Set(WF_STATUS_COMPLETED.into());
        am_inst.current_step_id = Set(None);
        am_inst.completed_at = Set(Some(now));
        am_inst.updated_at = Set(now);
        am_inst.update(&txn).await?;

        finalize_batch_approved(&txn, tenant_id, batch_id, now).await?;
        txn.commit().await?;
        return timesheet_week_batch::Entity::find_by_id(batch_id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("batch missing".into()));
    }

    approve_without_workflow_permission_check(
        &txn,
        tenant_id,
        batch_id,
        approver_user_id,
        batch.employee_id,
        now,
    )
    .await?;
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
    rejector_user_id: Uuid,
    rejection_reason: Option<String>,
) -> KabiPayResult<bool> {
    let txn = db.begin().await?;
    let batch = timesheet_week_batch::Entity::find_by_id(batch_id)
        .filter(timesheet_week_batch::Column::TenantId.eq(tenant_id))
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "timesheet_week_batch",
            id: batch_id.to_string(),
        })?;

    if batch.status != BATCH_PENDING {
        return Err(KabiPayError::Validation(
            "only pending submissions can be rejected".into(),
        ));
    }
    assert_actor_is_not_subject_employee(&txn, tenant_id, rejector_user_id, batch.employee_id)
        .await?;

    let now = Utc::now();

    if let Some(inst_id) = batch.workflow_instance_id {
        if let Some(mut inst) = workflow_instance::Entity::find_by_id(inst_id)
            .filter(workflow_instance::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
        {
            if inst.status == WF_STATUS_IN_PROGRESS {
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
                    workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
                        &txn,
                        tenant_id,
                        rejector_user_id,
                        batch.employee_id,
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
                am_inst.status = Set(WF_STATUS_CANCELLED.into());
                am_inst.completed_at = Set(Some(now));
                am_inst.updated_at = Set(now);
                am_inst.update(&txn).await?;
            }
        }
    } else if !workflow_approval::user_has_permission_via_roles(
        &txn,
        tenant_id,
        rejector_user_id,
        "timesheet",
        "approve",
    )
    .await?
    {
        return Err(KabiPayError::Forbidden(
            "timesheet reject requires timesheet:approve (or tenant HR role)".into(),
        ));
    }

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
    let sanitized_reason = rejection_reason.clone().and_then(|reason| {
        let trimmed = reason.trim().to_string();
        if trimmed.is_empty() {
            None
        } else {
            Some(trimmed)
        }
    });

    let mut am_batch: timesheet_week_batch::ActiveModel = batch.into();
    am_batch.status = Set(BATCH_REJECTED.into());
    am_batch.rejection_reason = Set(sanitized_reason.clone());
    am_batch.updated_at = Set(now);
    am_batch.update(&txn).await?;

    crate::services::timesheet_notification_service::notify_employee_timesheet_rejected(
        &txn,
        tenant_id,
        rejected_employee_id,
        rejected_week_start,
        sanitized_reason.as_deref(),
        now,
    )
    .await?;

    txn.commit().await?;
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

pub async fn resolve_timesheet_pending_approval_stage(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: &str,
    workflow_instance_id: Option<Uuid>,
) -> KabiPayResult<Option<String>> {
    workflow_inbox::pending_workflow_step_title(
        db,
        tenant_id,
        status,
        BATCH_PENDING_STATUS,
        workflow_instance_id,
    )
    .await
}

pub async fn timesheet_week_batch_viewer_may_approve(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    viewer_user_id: Uuid,
    status: &str,
    subject_employee_id: Uuid,
    workflow_instance_id: Option<Uuid>,
) -> KabiPayResult<bool> {
    if let Some(subject) = employee::Entity::find_by_id(subject_employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await?
    {
        if subject.user_id == Some(viewer_user_id) {
            return Ok(false);
        }
    }

    workflow_inbox::viewer_may_approve_pending_row(
        db,
        tenant_id,
        viewer_user_id,
        subject_employee_id,
        status,
        BATCH_PENDING_STATUS,
        workflow_instance_id,
        WorkflowActorCheckMode::TimesheetReportingManagerFallback,
        NoWorkflowInstanceGate::ResourceAction {
            resource: "timesheet",
            action: "approve",
        },
    )
    .await
}
