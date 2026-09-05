//! Tenant-scoped SeaORM queries for workflow definitions and runtime.

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0025_workflow::{workflow, workflow_action, workflow_instance, workflow_step};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

fn approval_permission(entity: &str) -> KabiPayResult<&'static str> {
    match entity {
        "LEAVE_REQUEST" => Ok("leave:approve"),
        "EXPENSE" => Ok("expense:approve"),
        "TRAVEL_REQUEST" => Ok("travel:approve"),
        "TIMESHEET_WEEK_BATCH" => Ok("timesheet:approve"),
        _ => Err(KabiPayError::Validation(
            "Choose Leave, Expenses, Travel, or Timesheets for approval.".into(),
        )),
    }
}

fn normalize_approver(value: &str) -> KabiPayResult<String> {
    match value.trim().to_ascii_uppercase().as_str() {
        "MANAGER" | "LINE_MANAGER" | "REPORTING_MANAGER" => Ok("REPORTING_MANAGER".into()),
        "PERMISSION" | "ROLE" => Ok("PERMISSION".into()),
        "REPORTING_MANAGER_OR_PERMISSION"
        | "REPORTING_MANAGER_OR_ROLE"
        | "MANAGER_OR_ROLE"
        | "MANAGER_OR_PERMISSION" => Ok("REPORTING_MANAGER_OR_PERMISSION".into()),
        _ => Err(KabiPayError::Validation(
            "Choose a reporting manager or an eligible approver.".into(),
        )),
    }
}

pub async fn list_workflows(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<workflow::Model>> {
    let limit = limit.clamp(1, 200);
    workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::IsActive.eq(true))
        .order_by_asc(workflow::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_instances(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<workflow_instance::Model>> {
    let limit = limit.clamp(1, 500);
    workflow_instance::Entity::find()
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .order_by_desc(workflow_instance::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Ordered steps for a workflow (definition).
pub async fn list_workflow_steps(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> KabiPayResult<Vec<workflow_step::Model>> {
    workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn get_workflow(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    workflow_id: Uuid,
) -> KabiPayResult<Option<workflow::Model>> {
    workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::Id.eq(workflow_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

/// Create a new approval definition after resolver permission enforcement.
pub async fn create_workflow(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    name: String,
    entity_type: String,
    is_active: bool,
    initial_approver_type: Option<String>,
) -> KabiPayResult<workflow::Model> {
    use sea_orm::{ConnectionTrait, DbBackend, Statement};
    let entity_type = entity_type.trim().to_ascii_uppercase();
    let permission = approval_permission(&entity_type)?;
    let first_approver = initial_approver_type
        .as_deref()
        .map(normalize_approver)
        .transpose()?;
    let txn = db.begin().await?;
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("workflow-definition:{tenant_id}:{entity_type}").into()],
    ))
    .await?;
    let active_exists = workflow::Entity::find()
        .filter(workflow::Column::TenantId.eq(tenant_id))
        .filter(workflow::Column::EntityType.eq(&entity_type))
        .filter(workflow::Column::IsActive.eq(true))
        .one(&txn)
        .await?
        .is_some();
    if is_active && active_exists {
        return Err(KabiPayError::BusinessRule {
            code: "WORKFLOW_ALREADY_CONFIGURED",
            message: "An active approval workflow already exists for this request type.".into(),
        });
    }
    let id = Uuid::new_v4();
    let now = Utc::now();
    let m = workflow::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        name: Set(name),
        entity_type: Set(entity_type),
        is_active: Set(is_active),
        created_at: Set(now),
        updated_at: Set(now),
    };
    let created = m.insert(&txn).await?;
    if let Some(approver) = first_approver {
        workflow_step::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            workflow_id: Set(id),
            sequence_order: Set(1),
            step_name: Set("Approval".into()),
            approver_type: Set(Some(approver)),
            approver_role_id: Set(None),
            approver_permission: Set(Some(permission.into())),
            can_skip: Set(false),
            sla_hours: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await?;
    }
    txn.commit().await?;
    Ok(created)
}

/// Append a **step** to a definition. Fails if `workflow_id` is not in tenant.
pub async fn create_workflow_step(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    workflow_id: Uuid,
    sequence_order: i32,
    step_name: String,
    approver_type: Option<String>,
    approver_role_id: Option<Uuid>,
    approver_permission: Option<String>,
    can_skip: bool,
    sla_hours: Option<i32>,
) -> KabiPayResult<workflow_step::Model> {
    let workflow = get_workflow(db, tenant_id, workflow_id).await?.ok_or_else(|| {
        KabiPayError::NotFound {
            entity: "workflow",
            id: workflow_id.to_string(),
        }
    })?;
    let inferred_permission = match workflow.entity_type.trim().to_ascii_uppercase().as_str() {
        "LEAVE_REQUEST" => Some("leave:approve"),
        "EXPENSE" => Some("expense:approve"),
        "TRAVEL_REQUEST" => Some("travel:approve"),
        "TIMESHEET_WEEK_BATCH" => Some("timesheet:approve"),
        _ => None,
    };
    let approver_permission = approver_permission
        .and_then(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            (!normalized.is_empty()).then_some(normalized)
        })
        .or_else(|| inferred_permission.map(str::to_owned));
    if sequence_order < 1 || sla_hours.is_some_and(|hours| hours < 1) {
        return Err(KabiPayError::Validation("Step order and response time must be positive.".into()));
    }
    if approver_permission.as_deref() != Some(approval_permission(&workflow.entity_type)?) {
        return Err(KabiPayError::Validation("The approver permission must match this workflow's request type.".into()));
    }
    let approver_type = Some(normalize_approver(
        approver_type.as_deref().unwrap_or("REPORTING_MANAGER"),
    )?);
    let permission_type = matches!(
        approver_type.as_deref(),
        Some("PERMISSION" | "REPORTING_MANAGER_OR_PERMISSION" | "MANAGER_OR_PERMISSION")
    );
    if permission_type && approver_permission.is_none() {
        return Err(KabiPayError::Validation(
            "permission-based workflow steps require approverPermission".into(),
        ));
    }
    let dupe = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
        .filter(workflow_step::Column::SequenceOrder.eq(sequence_order))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    if dupe.is_some() {
        return Err(KabiPayError::Validation(format!(
            "workflow step with sequence_order {sequence_order} already exists for this workflow"
        )));
    }

    let id = Uuid::new_v4();
    let now = Utc::now();
    let m = workflow_step::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        workflow_id: Set(workflow_id),
        sequence_order: Set(sequence_order),
        step_name: Set(step_name),
        approver_type: Set(approver_type),
        approver_role_id: Set(if permission_type { None } else { approver_role_id }),
        approver_permission: Set(approver_permission),
        can_skip: Set(can_skip),
        sla_hours: Set(sla_hours),
        created_at: Set(now),
        updated_at: Set(now),
    };
    m.insert(db).await.map_err(KabiPayError::from)
}

/// Remove a **definition** step when it has no **`workflow_action`** history (FK RESTRICT on `workflow_action.workflow_step_id`).
/// Active instances with this step as **`current_step_id`** get **`SET NULL`** when the row is deleted.
pub async fn delete_workflow_step(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    step_id: Uuid,
) -> KabiPayResult<()> {
    let step = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::Id.eq(step_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let _ = step.ok_or_else(|| KabiPayError::NotFound {
        entity: "workflow_step",
        id: step_id.to_string(),
    })?;

    let action_count = workflow_action::Entity::find()
        .filter(workflow_action::Column::TenantId.eq(tenant_id))
        .filter(workflow_action::Column::WorkflowStepId.eq(step_id))
        .count(db)
        .await
        .map_err(KabiPayError::from)?;
    if action_count > 0 {
        return Err(KabiPayError::Conflict(
            "cannot delete workflow step that has approval or runtime action history".into(),
        ));
    }

    workflow_step::Entity::delete_many()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::Id.eq(step_id))
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(())
}

/// Re-assign **`sequence_order`** (1 … *n*) in the order given. Uses a temporary range so **`uq_workflow_step_workflow_seq`** is never violated mid-update.
///
/// **`ordered_step_ids`** must contain **every** step id for **`workflow_id`**, each **once**.
pub async fn reorder_workflow_steps(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    workflow_id: Uuid,
    ordered_step_ids: Vec<Uuid>,
) -> KabiPayResult<Vec<workflow_step::Model>> {
    if get_workflow(db, tenant_id, workflow_id).await?.is_none() {
        return Err(KabiPayError::NotFound {
            entity: "workflow",
            id: workflow_id.to_string(),
        });
    }

    let existing = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    if existing.is_empty() {
        return if ordered_step_ids.is_empty() {
            Ok(vec![])
        } else {
            Err(KabiPayError::Validation(
                "orderedStepIds must be empty when the workflow has no steps".into(),
            ))
        };
    }

    let mut claimed = std::collections::HashMap::with_capacity(existing.len());
    for s in existing {
        claimed.insert(s.id, s);
    }

    let n = claimed.len();
    if ordered_step_ids.len() != n {
        return Err(KabiPayError::Validation(format!(
            "ordered step count {} does not match workflow step count {}",
            ordered_step_ids.len(),
            n
        )));
    }

    let mut seen = std::collections::HashSet::with_capacity(n);
    for id in &ordered_step_ids {
        if !seen.insert(id) {
            return Err(KabiPayError::Validation("duplicate step id in orderedStepIds".into()));
        }
        if !claimed.contains_key(id) {
            return Err(KabiPayError::Validation(format!(
                "step {id} is not part of this workflow"
            )));
        }
    }

    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let max_seq = claimed
        .values()
        .map(|s| s.sequence_order)
        .max()
        .unwrap_or(0);
    let temp_base = max_seq.max(1) + 10_000;

    for (i, sid) in ordered_step_ids.iter().enumerate() {
        let step_model = claimed
            .remove(sid)
            .expect("validated contains id");
        let mut am = step_model.into_active_model();
        am.sequence_order = Set(temp_base + i as i32);
        am.update(&txn).await.map_err(KabiPayError::from)?;
    }

    for (i, sid) in ordered_step_ids.iter().enumerate() {
        let seq = i as i32 + 1;
        let row = workflow_step::Entity::find_by_id(*sid)
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| {
                KabiPayError::Internal("workflow_step missing after reorder phase 1".into())
            })?;
        let mut am = row.into_active_model();
        am.sequence_order = Set(seq);
        am.update(&txn).await.map_err(KabiPayError::from)?;
    }

    txn.commit().await.map_err(KabiPayError::from)?;

    list_workflow_steps(db, tenant_id, workflow_id).await
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn supported_request_types_map_to_their_exact_approval_permission() {
        assert_eq!(approval_permission("LEAVE_REQUEST").unwrap(), "leave:approve");
        assert_eq!(approval_permission("EXPENSE").unwrap(), "expense:approve");
        assert_eq!(approval_permission("TRAVEL_REQUEST").unwrap(), "travel:approve");
        assert_eq!(
            approval_permission("TIMESHEET_WEEK_BATCH").unwrap(),
            "timesheet:approve"
        );
        assert!(matches!(
            approval_permission("UNKNOWN"),
            Err(KabiPayError::Validation(_))
        ));
    }

    #[test]
    fn legacy_approver_names_normalize_to_permission_based_runtime_values() {
        assert_eq!(normalize_approver("manager").unwrap(), "REPORTING_MANAGER");
        assert_eq!(normalize_approver("role").unwrap(), "PERMISSION");
        assert_eq!(
            normalize_approver("manager_or_role").unwrap(),
            "REPORTING_MANAGER_OR_PERMISSION"
        );
        assert!(matches!(
            normalize_approver("accountant"),
            Err(KabiPayError::Validation(_))
        ));
    }
}
