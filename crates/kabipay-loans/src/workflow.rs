use crate::{
    access, repository as store, LoanActorScope, LoanModuleError, LoanResult, RequestDecision,
};
use chrono::Utc;
use kabipay_common::workflow_approval::{assert_workflow_step_actor, WorkflowApprovalAuthority};
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow, workflow_action, workflow_instance, workflow_step,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set,
};
use uuid::Uuid;

pub(crate) async fn configured(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
) -> LoanResult<workflow_step::Model> {
    let wf = workflow::Entity::find_by_id(id)
        .filter(workflow::Column::TenantId.eq(tenant))
        .one(tx)
        .await?
        .ok_or(LoanModuleError::PolicyUnavailable)?;
    if !wf.is_active || wf.entity_type != "LOAN_REQUEST" {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    let steps = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant))
        .filter(workflow_step::Column::WorkflowId.eq(id))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .all(tx)
        .await?;
    if steps.is_empty()
        || steps
            .iter()
            .any(|step| step.approver_permission.as_deref() != Some("loan:approve"))
    {
        return Err(LoanModuleError::PolicyUnavailable);
    }
    steps
        .into_iter()
        .next()
        .ok_or(LoanModuleError::PolicyUnavailable)
}
pub(crate) async fn attach(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    request: Uuid,
    workflow_id: Uuid,
) -> LoanResult<Uuid> {
    let first = configured(tx, actor.tenant_id, workflow_id).await?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    workflow_instance::ActiveModel {
        id: Set(id),
        tenant_id: Set(actor.tenant_id),
        workflow_id: Set(workflow_id),
        entity_type: Set("LOAN_REQUEST".into()),
        entity_id: Set(request),
        status: Set("IN_PROGRESS".into()),
        current_step_id: Set(Some(first.id)),
        created_at: Set(now),
        completed_at: Set(None),
        updated_at: Set(now),
    }
    .insert(tx)
    .await?;
    Ok(id)
}
/// Returns true only on the configured final approval step. It never grants financial terms.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn decide(
    tx: &DatabaseTransaction,
    actor: &LoanActorScope,
    employee: Uuid,
    request: Uuid,
    instance_id: Uuid,
    step_id: Uuid,
    decision: RequestDecision,
    reason: &str,
) -> LoanResult<bool> {
    let instance = workflow_instance::Entity::find_by_id(instance_id)
        .filter(workflow_instance::Column::TenantId.eq(actor.tenant_id))
        .lock_exclusive()
        .one(tx)
        .await?
        .ok_or(LoanModuleError::NotFound)?;
    if instance.entity_type != "LOAN_REQUEST"
        || instance.entity_id != request
        || instance.status != "IN_PROGRESS"
        || instance.current_step_id != Some(step_id)
    {
        return Err(LoanModuleError::VersionConflict);
    }
    configured(tx, actor.tenant_id, instance.workflow_id).await?;
    let step = workflow_step::Entity::find_by_id(step_id)
        .filter(workflow_step::Column::TenantId.eq(actor.tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(instance.workflow_id))
        .lock_exclusive()
        .one(tx)
        .await?
        .ok_or(LoanModuleError::VersionConflict)?;
    let authority = WorkflowApprovalAuthority {
        actor_user_id: actor.user_id,
        actor_employee: access::viewer(tx, actor).await?,
        scope: actor.scope,
        permission: "loan:approve",
    };
    assert_workflow_step_actor(tx, actor.tenant_id, employee, &step, &authority).await?;
    let next = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(actor.tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(instance.workflow_id))
        .filter(workflow_step::Column::SequenceOrder.gt(step.sequence_order))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .one(tx)
        .await?;
    let final_approval = matches!(decision, RequestDecision::Approve) && next.is_none();
    let action = match decision {
        RequestDecision::Approve => "APPROVE",
        RequestDecision::Return => "RETURN",
        RequestDecision::Reject => "REJECT",
    };
    let now = Utc::now();
    workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(actor.tenant_id),
        instance_id: Set(instance_id),
        workflow_step_id: Set(step_id),
        performed_by: Set(Some(actor.user_id)),
        action: Set(action.into()),
        remarks: Set(Some(reason.into())),
        acted_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(tx)
    .await?;
    let mut model: workflow_instance::ActiveModel = instance.into();
    if matches!(decision, RequestDecision::Approve) && !final_approval {
        model.current_step_id = Set(next.map(|s| s.id));
    } else {
        model.status = Set(match decision {
            RequestDecision::Approve => "COMPLETED",
            RequestDecision::Return => "RETURNED",
            RequestDecision::Reject => "REJECTED",
        }
        .into());
        model.current_step_id = Set(None);
        model.completed_at = Set(Some(now));
    }
    model.updated_at = Set(now);
    model.update(tx).await?;
    Ok(final_approval)
}
pub(crate) async fn withdraw(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    instance: Uuid,
) -> LoanResult<()> {
    store::execute(tx,"UPDATE workflow_instance SET status='CANCELLED',current_step_id=NULL,completed_at=NOW(),updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND status='IN_PROGRESS'",vec![tenant.into(),instance.into()]).await?;
    Ok(())
}
