//! Tenant-scoped travel requests (M14); multi-step workflow parity with expense claims (M32).

use chrono::Utc;
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use kabipay_common::workflow_approval;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow, workflow_action, workflow_instance, workflow_step,
};
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use kabipay_db_entities::tenant::d0033_travel_request::travel_request;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use sea_orm::sea_query::LockType;
use uuid::Uuid;

use super::approval_authority::{
    after_successful_commit, lock_and_validate_decision_employees, lock_current_workflow,
    workflow_authority, ExpenseApprovalAuthority,
};

const STATUS_PENDING: &str = "PENDING";
const STATUS_APPROVED: &str = "APPROVED";
const STATUS_REJECTED: &str = "REJECTED";

/// Matches `workflow.entity_type` / `workflow_instance.entity_type` for travel (**M32**).
pub const WF_ENTITY_TRAVEL_REQUEST: &str = "TRAVEL_REQUEST";

const WF_STATUS_IN_PROGRESS: &str = "IN_PROGRESS";
const WF_STATUS_COMPLETED: &str = "COMPLETED";
const WF_STATUS_CANCELLED: &str = "CANCELLED";
const WF_ACTION_APPROVE: &str = "APPROVE";
const WF_ACTION_REJECT: &str = "REJECT";

pub async fn list_travel_requests(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    scope_filter: &EmployeeScopeFilter,
) -> KabiPayResult<Vec<travel_request::Model>> {
    let limit = limit.clamp(1, 200);
    match scope_filter {
        EmployeeScopeFilter::Empty => return Ok(vec![]),
        EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(vec![]),
        _ => {}
    }
    let mut q = travel_request::Entity::find().filter(travel_request::Column::TenantId.eq(tenant_id));
    if let EmployeeScopeFilter::EmployeeIds(ids) = scope_filter {
        q = q.filter(travel_request::Column::EmployeeId.is_in(ids.clone()));
    }
    q.order_by_desc(travel_request::Column::SubmittedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn submit_travel_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    origin_location: Option<String>,
    destination_location: Option<String>,
    from_date: chrono::NaiveDate,
    to_date: chrono::NaiveDate,
    purpose: &str,
    estimated_amount: Option<Decimal>,
    currency: &str,
) -> KabiPayResult<travel_request::Model> {
    if from_date > to_date {
        return Err(KabiPayError::Validation(
            "from_date must be on or before to_date".into(),
        ));
    }
    if purpose.trim().is_empty() {
        return Err(KabiPayError::Validation("purpose is required".into()));
    }
    if let Some(a) = estimated_amount {
        if a < Decimal::ZERO {
            return Err(KabiPayError::Validation(
                "estimated_amount cannot be negative".into(),
            ));
        }
    }
    let txn = db.begin().await?;
    let id = Uuid::new_v4();
    let now = Utc::now();
    let am = travel_request::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        origin_location: Set(origin_location.filter(|s| !s.is_empty())),
        destination_location: Set(destination_location.filter(|s| !s.is_empty())),
        from_date: Set(from_date),
        to_date: Set(to_date),
        purpose: Set(purpose.trim().to_string()),
        estimated_amount: Set(estimated_amount),
        currency: Set(currency.to_string()),
        status: Set(STATUS_PENDING.into()),
        rejection_reason: Set(None),
        approved_by: Set(None),
        rejected_by: Set(None),
        workflow_instance_id: Set(None),
        submitted_at: Set(now),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(&txn).await?;
    try_attach_travel_workflow(&txn, tenant_id, id, now).await?;

    txn.commit().await?;

    travel_request::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted travel_request not found".into()))
}

async fn load_travel_workflow_first_step(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
) -> KabiPayResult<Option<(workflow::Model, Uuid)>> {
    let wf = workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::IsActive.eq(true))
        .filter(workflow::Column::EntityType.eq(WF_ENTITY_TRAVEL_REQUEST))
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

async fn try_attach_travel_workflow(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    travel_request_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let Some((wf, first_step_id)) = load_travel_workflow_first_step(txn, tenant_id).await? else {
        return Ok(());
    };
    let inst_id = Uuid::new_v4();
    let inst = workflow_instance::ActiveModel {
        id: Set(inst_id),
        tenant_id: Set(tenant_id),
        workflow_id: Set(wf.id),
        entity_type: Set(WF_ENTITY_TRAVEL_REQUEST.into()),
        entity_id: Set(travel_request_id),
        status: Set(WF_STATUS_IN_PROGRESS.into()),
        current_step_id: Set(Some(first_step_id)),
        created_at: Set(now),
        completed_at: Set(None),
        updated_at: Set(now),
    };
    inst.insert(txn).await.map_err(KabiPayError::from)?;

    let mut am_req: travel_request::ActiveModel = travel_request::Entity::find_by_id(travel_request_id)
        .one(txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("travel_request missing after insert".into()))?
        .into();
    am_req.workflow_instance_id = Set(Some(inst_id));
    am_req.update(txn).await.map_err(KabiPayError::from)?;
    Ok(())
}

async fn finalize_travel_approval(
    txn: &impl ConnectionTrait,
    tenant_id: Uuid,
    travel_request_id: Uuid,
    approver_user_id: Uuid,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<travel_request::Model> {
    let m = travel_request::Entity::find()
        .filter(travel_request::Column::Id.eq(travel_request_id))
        .filter(travel_request::Column::TenantId.eq(tenant_id))
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "travel_request",
            id: travel_request_id.to_string(),
        })?;
    let mut am: travel_request::ActiveModel = m.into();
    am.status = Set(STATUS_APPROVED.into());
    am.rejection_reason = Set(None);
    am.approved_by = Set(Some(approver_user_id));
    am.rejected_by = Set(None);
    am.updated_at = Set(now);
    am.update(txn).await?;
    travel_request::Entity::find_by_id(travel_request_id)
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated travel_request not found".into()))
}

pub async fn approve_travel_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    travel_request_id: Uuid,
    expected_workflow_step_id: Uuid,
    authority: &ExpenseApprovalAuthority,
) -> KabiPayResult<travel_request::Model> {
    let txn = db.begin().await?;
    let model = lock_pending_travel(&txn, tenant_id, travel_request_id).await?;
    let (actor, _subject) = lock_and_validate_decision_employees(
        &txn, tenant_id, authority, model.employee_id,
    )
    .await?;
    let action_authority = workflow_authority(authority, actor);
    let now = Utc::now();
    let (instance, current_step) = lock_current_workflow(
        &txn,
        tenant_id,
        WF_ENTITY_TRAVEL_REQUEST,
        travel_request_id,
        model.workflow_instance_id,
        expected_workflow_step_id,
    )
    .await?;
    workflow_approval::assert_workflow_step_actor(
        &txn, tenant_id, model.employee_id, &current_step, &action_authority,
    )
    .await?;
    workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        instance_id: Set(instance.id),
        workflow_step_id: Set(current_step.id),
        performed_by: Set(Some(authority.actor_user_id)),
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
        .lock_exclusive()
        .one(&txn)
        .await?;
    if let Some(next) = next_step {
        let mut active_instance: workflow_instance::ActiveModel = instance.into();
        active_instance.current_step_id = Set(Some(next.id));
        active_instance.updated_at = Set(now);
        active_instance.update(&txn).await?;
        txn.commit().await?;
        return travel_request::Entity::find_by_id(travel_request_id)
            .one(db)
            .await?
            .ok_or_else(|| {
                KabiPayError::Internal("travel_request missing after workflow step".into())
            });
    }
    let mut active_instance: workflow_instance::ActiveModel = instance.into();
    active_instance.status = Set(WF_STATUS_COMPLETED.into());
    active_instance.current_step_id = Set(None);
    active_instance.completed_at = Set(Some(now));
    active_instance.updated_at = Set(now);
    active_instance.update(&txn).await?;
    finalize_travel_approval(
        &txn,
        tenant_id,
        travel_request_id,
        authority.actor_user_id,
        now,
    )
    .await?;
    after_successful_commit(txn.commit(), || async {
        let out = travel_request::Entity::find_by_id(travel_request_id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("travel_request missing after approve".into()))?;
        travel_notify_employee(
            db,
            tenant_id,
            out.employee_id,
            "Travel request approved",
            "Your travel request was approved.",
        )
        .await;
        Ok(out)
    })
    .await
}

pub async fn reject_travel_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    travel_request_id: Uuid,
    expected_workflow_step_id: Uuid,
    authority: &ExpenseApprovalAuthority,
    rejection_reason: Option<String>,
) -> KabiPayResult<travel_request::Model> {
    let txn = db.begin().await?;
    let model = lock_pending_travel(&txn, tenant_id, travel_request_id).await?;
    let (actor, _subject) = lock_and_validate_decision_employees(
        &txn, tenant_id, authority, model.employee_id,
    )
    .await?;
    let action_authority = workflow_authority(authority, actor);
    let (instance, current_step) = lock_current_workflow(
        &txn,
        tenant_id,
        WF_ENTITY_TRAVEL_REQUEST,
        travel_request_id,
        model.workflow_instance_id,
        expected_workflow_step_id,
    )
    .await?;
    workflow_approval::assert_workflow_step_actor(
        &txn, tenant_id, model.employee_id, &current_step, &action_authority,
    )
    .await?;
    let now = Utc::now();
    workflow_action::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        instance_id: Set(instance.id),
        workflow_step_id: Set(current_step.id),
        performed_by: Set(Some(authority.actor_user_id)),
        action: Set(WF_ACTION_REJECT.into()),
        remarks: Set(rejection_reason.clone()),
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
    let mut active_request: travel_request::ActiveModel = model.into();
    active_request.status = Set(STATUS_REJECTED.into());
    active_request.rejection_reason = Set(rejection_reason.clone());
    active_request.approved_by = Set(None);
    active_request.rejected_by = Set(Some(authority.actor_user_id));
    active_request.updated_at = Set(now);
    active_request.update(&txn).await?;
    let out = travel_request::Entity::find_by_id(travel_request_id)
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("updated travel_request not found".into()))?;
    after_successful_commit(txn.commit(), || async {
        let msg = format!(
            "Your travel request was rejected.{}",
            out.rejection_reason
                .as_ref()
                .filter(|reason| !reason.is_empty())
                .map(|reason| format!(" Reason: {reason}"))
                .unwrap_or_default()
        );
        travel_notify_employee(db, tenant_id, out.employee_id, "Travel request rejected", &msg)
            .await;
        Ok(out)
    })
    .await
}

async fn lock_pending_travel(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    id: Uuid,
) -> KabiPayResult<travel_request::Model> {
    let model = pending_travel_for_decision_query(id, tenant_id)
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "travel_request",
            id: id.to_string(),
        })?;
    if model.status != STATUS_PENDING {
        return Err(KabiPayError::Conflict(
            "travel request is no longer pending; refresh before taking another action".into(),
        ));
    }
    Ok(model)
}

fn pending_travel_for_decision_query(
    id: Uuid,
    tenant_id: Uuid,
) -> sea_orm::Select<travel_request::Entity> {
    travel_request::Entity::find()
        .filter(travel_request::Column::Id.eq(id))
        .filter(travel_request::Column::TenantId.eq(tenant_id))
        .lock(LockType::Update)
}

#[cfg(test)]
mod approval_decision_tests {
    use super::*;
    use sea_orm::{DbBackend, QueryTrait};

    #[test]
    fn travel_decision_locks_only_the_tenant_request() {
        let sql = pending_travel_for_decision_query(Uuid::new_v4(), Uuid::new_v4())
            .build(DbBackend::Postgres)
            .to_string();
        assert!(sql.ends_with("FOR UPDATE"));
    }
}

async fn travel_notify_employee(
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
        r#type: Set(Some("TRAVEL".into())),
        title: Set(Some(title.into())),
        message: Set(Some(message.into())),
        action_url: Set(None),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    if let Err(e) = am.insert(db).await {
        tracing::warn!(error = %e, "insert notification (travel) failed");
    }
}
