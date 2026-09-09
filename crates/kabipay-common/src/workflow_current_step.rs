//! Repair `workflow_instance.current_step_id` when it is missing or orphaned.
//!
//! `workflow_instance.current_step_id` references `workflow_step.id` with **`ON DELETE SET NULL`**
//! (see migration `0025_workflow`). Deleting or replacing a step therefore clears the pointer while
//! the instance can remain **`IN_PROGRESS`**, which blocks approve/reject. We recover by picking the
//! first step (by `sequence_order`) that has no **`APPROVE`** [`workflow_action`] yet for this instance.

use std::collections::HashSet;

use chrono::{DateTime, Utc};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder, Set,
};
use uuid::Uuid;

use crate::error::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0025_workflow::{
    workflow_action, workflow_instance, workflow_step,
};

const WF_ACTION_APPROVE: &str = "APPROVE";

async fn resolve_first_step_without_approve_action(
    conn: &impl ConnectionTrait,
    tenant_id: Uuid,
    instance_id: Uuid,
    workflow_id: Uuid,
) -> KabiPayResult<Option<workflow_step::Model>> {
    let steps = workflow_step::Entity::find()
        .filter(workflow_step::Column::TenantId.eq(tenant_id))
        .filter(workflow_step::Column::WorkflowId.eq(workflow_id))
        .order_by_asc(workflow_step::Column::SequenceOrder)
        .all(conn)
        .await?;

    if steps.is_empty() {
        return Ok(None);
    }

    let approved: HashSet<Uuid> = workflow_action::Entity::find()
        .filter(workflow_action::Column::InstanceId.eq(instance_id))
        .filter(workflow_action::Column::Action.eq(WF_ACTION_APPROVE))
        .all(conn)
        .await?
        .into_iter()
        .map(|a| a.workflow_step_id)
        .collect();

    Ok(steps.into_iter().find(|s| !approved.contains(&s.id)))
}

/// Read-only: resolve which workflow step logically applies now (does **not** mutate the instance row).
///
/// Used for list/UI clarity and **`viewerMayApprove`** previews without writing on **GET**.
/// Mutations continue to persist repair via [`ensure_workflow_instance_current_step_repaired`].
pub async fn resolve_logical_current_workflow_step(
    conn: &impl ConnectionTrait,
    tenant_id: Uuid,
    inst: &workflow_instance::Model,
) -> KabiPayResult<Option<workflow_step::Model>> {
    if let Some(sid) = inst.current_step_id {
        if let Some(step) = workflow_step::Entity::find_by_id(sid)
            .filter(workflow_step::Column::TenantId.eq(tenant_id))
            .one(conn)
            .await?
        {
            return Ok(Some(step));
        }
    }
    resolve_first_step_without_approve_action(conn, tenant_id, inst.id, inst.workflow_id).await
}

/// When **`current_step_id`** is **`NULL`** or points at a deleted step, set it to the next pending
/// step derived from **`workflow_action`** history (first step without an **`APPROVE`** row).
///
/// No-op when the pointer already refers to an existing step for this tenant.
pub async fn ensure_workflow_instance_current_step_repaired(
    conn: &impl ConnectionTrait,
    tenant_id: Uuid,
    inst: &workflow_instance::Model,
    now: DateTime<Utc>,
) -> KabiPayResult<workflow_instance::Model> {
    let mut needs_repair = inst.current_step_id.is_none();
    if let Some(sid) = inst.current_step_id {
        let exists = workflow_step::Entity::find_by_id(sid)
            .filter(workflow_step::Column::TenantId.eq(tenant_id))
            .one(conn)
            .await?
            .is_some();
        if !exists {
            needs_repair = true;
        }
    }

    if !needs_repair {
        return Ok(inst.clone());
    }

    let resolved =
        resolve_first_step_without_approve_action(conn, tenant_id, inst.id, inst.workflow_id).await?;
    let Some(step) = resolved else {
        tracing::error!(
            tenant_id = %tenant_id,
            instance_id = %inst.id,
            workflow_id = %inst.workflow_id,
            "workflow_instance missing current_step_id and could not derive a pending step"
        );
        return Err(KabiPayError::Validation(
            "workflow instance has no current step — a workflow step may have been removed while this request was pending. Ask an admin to fix the workflow definition or cancel and resubmit."
                .into(),
        ));
    };

    tracing::warn!(
        tenant_id = %tenant_id,
        instance_id = %inst.id,
        repaired_step_id = %step.id,
        "repaired workflow_instance.current_step_id after NULL/orphan pointer (often caused by deleting a workflow step)"
    );

    let mut am: workflow_instance::ActiveModel = inst.clone().into();
    am.current_step_id = Set(Some(step.id));
    am.updated_at = Set(now);
    am.update(conn).await?;

    workflow_instance::Entity::find_by_id(inst.id)
        .filter(workflow_instance::Column::TenantId.eq(tenant_id))
        .one(conn)
        .await?
        .ok_or_else(|| KabiPayError::Internal("workflow_instance missing after repair update".into()))
}

/// Batch equivalent of logical step resolution for queue display. LATERAL bounds the
/// result to one step per instance even for long workflows and large action histories.
/// This is display data only; action authority still requires a valid current pointer.
pub async fn pending_step_titles_batch(
    conn: &impl ConnectionTrait,
    tenant_id: Uuid,
    instance_ids: &[Uuid],
) -> KabiPayResult<std::collections::HashMap<Uuid, String>> {
    use sea_orm::{DbBackend, FromQueryResult, Statement};
    #[derive(FromQueryResult)]
    struct Title {
        id: Uuid,
        status: String,
        step_name: String,
    }
    if instance_ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let placeholders = (2..=instance_ids.len() + 1)
        .map(|index| format!("${index}"))
        .collect::<Vec<_>>()
        .join(",");
    let sql = format!(r#"
        SELECT i.id, i.status, COALESCE(current_step.step_name, fallback.step_name) AS step_name
        FROM workflow_instance i
        LEFT JOIN workflow_step current_step ON current_step.id = i.current_step_id AND current_step.tenant_id = $1
        LEFT JOIN LATERAL (
            SELECT s.step_name FROM workflow_step s
            WHERE current_step.id IS NULL AND s.tenant_id = $1 AND s.workflow_id = i.workflow_id
              AND NOT EXISTS (SELECT 1 FROM workflow_action a WHERE a.instance_id = i.id AND a.workflow_step_id = s.id AND a.action = 'APPROVE')
            ORDER BY s.sequence_order ASC LIMIT 1
        ) fallback ON TRUE
        WHERE i.tenant_id = $1 AND i.id IN ({placeholders})
          AND COALESCE(current_step.step_name, fallback.step_name) IS NOT NULL
    "#);
    let values = std::iter::once(tenant_id.into())
        .chain(instance_ids.iter().map(|id| (*id).into()));
    Ok(Title::find_by_statement(Statement::from_sql_and_values(
        DbBackend::Postgres, sql, values,
    ))
    .all(conn)
    .await?
    .into_iter()
    .filter(|row| row.status.trim().eq_ignore_ascii_case("IN_PROGRESS"))
    .map(|row| (row.id, row.step_name))
    .collect())
}
