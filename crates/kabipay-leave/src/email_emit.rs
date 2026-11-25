//! Called only inside the transaction which owns a Leave transition.
use chrono::{DateTime, Utc};
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use kabipay_organization_tenant::{
    email_emission::{emit_requester_outcome, emit_workflow_actions, WorkflowEmailActivation},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;

pub async fn emit_active_step(
    tx: &DatabaseTransaction,
    metadata: &DatabaseConnection,
    tenant: Uuid,
    request: Uuid,
    actor: EmailActorRef,
    at: DateTime<Utc>,
) -> KabiPayResult<usize> {
    emit_active_step_with_aliases(tx, metadata, tenant, request, actor, &[], at).await
}
pub async fn emit_active_step_with_aliases(
    tx: &DatabaseTransaction, metadata: &DatabaseConnection, tenant: Uuid,
    request: Uuid, actor: EmailActorRef, aliases: &[Uuid], at: DateTime<Utc>,
) -> KabiPayResult<usize> {
    let row = tx
        .query_one(statement(
            crate::email_alerts::EMAIL_RESOURCE_SQL,
            vec![tenant.into(), request.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::Internal("committed leave candidate is missing".into()))?;
    let status: String = row.try_get("", "status")?;
    if status != "PENDING" {
        return Ok(0);
    }
    emit_workflow_actions(
        tx,
        Some(metadata),
        WorkflowEmailActivation {
            transition: EmailTransition {
                tenant_id: tenant,
                resource_type: EmailResourceType::Leave,
                resource_id: request,
                resource_version: email_timestamp_version(&status, row.try_get("", "updated_at")?),
                occurred_at: at,
                actor_ref: Some(actor),
                actor_target_user_ids: aliases.to_vec(),
            },
            employee_id: row.try_get("", "employee_id")?,
            instance_id: row.try_get("", "workflow_instance_id")?,
            entity_type: "LEAVE_REQUEST",
            require_active_definition: false,
        },
    )
    .await
}
pub async fn emit_outcome(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    request: Uuid,
    actor: EmailActorRef,
    action: Option<Uuid>,
    at: DateTime<Utc>,
) -> KabiPayResult<usize> {
    let row = tx
        .query_one(statement(
            crate::email_alerts::EMAIL_RESOURCE_SQL,
            vec![tenant.into(), request.into()],
        ))
        .await?
        .ok_or_else(|| KabiPayError::Internal("committed leave candidate is missing".into()))?;
    let status: String = row.try_get("", "status")?;
    if !matches!(status.as_str(), "APPROVED" | "REJECTED" | "CANCELLED") {
        return Err(KabiPayError::Internal("leave outcome is not final".into()));
    }
    let key = workflow_email_outcome_key(&status, action, at);
    emit_requester_outcome(
        tx,
        EmailTransition {
            tenant_id: tenant,
            resource_type: EmailResourceType::Leave,
            resource_id: request,
            resource_version: key.clone(),
            occurred_at: at,
            actor_ref: Some(actor),
                actor_target_user_ids: vec![],
        },
        row.try_get("", "employee_id")?,
        key,
    )
    .await
}
