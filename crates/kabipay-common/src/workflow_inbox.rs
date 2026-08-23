//! Generic approval-queue UX: pending workflow step label + whether the current user may act.
//! Entity-specific crates pass status constants and actor-check mode (`expense`, `travel`, `timesheet`).

use uuid::Uuid;

use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};

use kabipay_db_entities::tenant::d0025_workflow::workflow_instance;

use crate::workflow_approval;
use crate::workflow_current_step;
use crate::{KabiPayResult};

/// Workflow instances that are accepting approve/reject.
pub const WF_INSTANCE_IN_PROGRESS: &str = "IN_PROGRESS";

/// `true` when `row_status` matches the entity's canonical pending literal (typically `PENDING`).
pub fn entity_row_is_pending(row_status: &str, pending_literal: &str) -> bool {
    row_status
        .trim()
        .eq_ignore_ascii_case(pending_literal.trim())
}

/// Workflow step **`step_name`** for display when **`row`** is pending and a workflow instance is in progress.
pub async fn pending_workflow_step_title(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    row_status: &str,
    pending_literal: &str,
    workflow_instance_id: Option<Uuid>,
) -> KabiPayResult<Option<String>> {
    if !entity_row_is_pending(row_status, pending_literal) {
        return Ok(None);
    }
    let Some(inst_id) = workflow_instance_id else {
        return Ok(None);
    };
    let Some(inst) = workflow_instance::Entity::find_by_id(inst_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
    else {
        return Ok(None);
    };
    if !inst
        .status
        .trim()
        .eq_ignore_ascii_case(WF_INSTANCE_IN_PROGRESS)
    {
        return Ok(None);
    }
    let Some(step) =
        workflow_current_step::resolve_logical_current_workflow_step(db, tenant_id, &inst).await?
    else {
        return Ok(None);
    };
    Ok(Some(step.step_name))
}

pub async fn workflow_instance_accepts_actions(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    workflow_instance_id: Uuid,
) -> KabiPayResult<bool> {
    let Some(inst) = workflow_instance::Entity::find_by_id(workflow_instance_id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
    else {
        return Ok(false);
    };
    if !inst
        .status
        .trim()
        .eq_ignore_ascii_case(WF_INSTANCE_IN_PROGRESS)
    {
        return Ok(false);
    }
    workflow_current_step::resolve_logical_current_workflow_step(db, tenant_id, &inst)
        .await
        .map(|step| step.is_some())
}

/// Mirrors [`crate::workflow_approval`] behaviour for **`assert_workflow_step_actor`** mutations.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum WorkflowActorCheckMode {
    Standard,
    /// Match [`workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback`].
    TimesheetReportingManagerFallback,
}

/// When there is **no** `workflow_instance_id`, how we decide if the viewer may approve legacy rows.
#[derive(Debug, Clone, Copy, Eq, PartialEq)]
pub enum NoWorkflowInstanceGate {
    ResourceAction {
        resource: &'static str,
        action: &'static str,
    },
    Travel,
}

/// **`true`** if this user may approve/reject **now** (`PENDING` + workflow step actor, or legacy gate).
pub async fn viewer_may_approve_pending_row(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    viewer_user_id: Uuid,
    subject_employee_id: Uuid,
    row_status: &str,
    pending_literal: &str,
    workflow_instance_id: Option<Uuid>,
    actor_mode: WorkflowActorCheckMode,
    no_instance_gate: NoWorkflowInstanceGate,
) -> KabiPayResult<bool> {
    if !entity_row_is_pending(row_status, pending_literal) {
        return Ok(false);
    }

    match workflow_instance_id {
        None => match no_instance_gate {
            NoWorkflowInstanceGate::ResourceAction { resource, action } => {
                workflow_approval::user_has_permission_via_roles(
                    db,
                    tenant_id,
                    viewer_user_id,
                    resource,
                    action,
                )
                .await
            }
            NoWorkflowInstanceGate::Travel => Ok(workflow_approval::assert_travel_approval_actor(
                db,
                tenant_id,
                viewer_user_id,
                subject_employee_id,
            )
            .await
            .is_ok()),
        },
        Some(inst_id) => {
            let Some(inst) = workflow_instance::Entity::find_by_id(inst_id)
                .filter(workflow_instance::Column::TenantId.eq(tenant_id))
                .one(db)
                .await?
            else {
                return Ok(false);
            };
            if !inst
                .status
                .trim()
                .eq_ignore_ascii_case(WF_INSTANCE_IN_PROGRESS)
            {
                return Ok(false);
            }
            let Some(step) = workflow_current_step::resolve_logical_current_workflow_step(
                db,
                tenant_id,
                &inst,
            )
            .await?
            else {
                return Ok(false);
            };

            let ok = match actor_mode {
                WorkflowActorCheckMode::Standard => workflow_approval::assert_workflow_step_actor(
                    db,
                    tenant_id,
                    viewer_user_id,
                    subject_employee_id,
                    &step,
                )
                .await
                .is_ok(),
                WorkflowActorCheckMode::TimesheetReportingManagerFallback => workflow_approval::assert_workflow_step_actor_with_timesheet_reporting_manager_fallback(
                    db,
                    tenant_id,
                    viewer_user_id,
                    subject_employee_id,
                    &step,
                )
                .await
                .is_ok(),
            };
            Ok(ok)
        }
    }
}
