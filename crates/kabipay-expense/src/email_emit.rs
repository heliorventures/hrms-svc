//! Expense and Travel use only their existing local workflow authority.
use chrono::{DateTime, Utc};
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use kabipay_organization_tenant::{
    email_emission::{emit_requester_outcome, emit_workflow_actions, WorkflowEmailActivation},
    statement,
};
use sea_orm::{ConnectionTrait, DatabaseTransaction, QueryResult};
use uuid::Uuid;
async fn resource(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
    kind: EmailResourceType,
) -> KabiPayResult<(QueryResult, &'static str)> {
    let (sql, entity) = match kind {
        EmailResourceType::Expense => (
            crate::email_alerts::EMAIL_EXPENSE_SQL,
            crate::services::expense_service::WF_ENTITY_EXPENSE,
        ),
        EmailResourceType::Travel => (
            crate::email_alerts::EMAIL_TRAVEL_SQL,
            crate::services::travel_request_service::WF_ENTITY_TRAVEL_REQUEST,
        ),
        _ => return Err(KabiPayError::Unauthorised),
    };
    Ok((
        tx.query_one(statement(sql, vec![tenant.into(), id.into()]))
            .await?
            .ok_or_else(|| {
                KabiPayError::Internal("committed approval candidate is missing".into())
            })?,
        entity,
    ))
}
pub async fn emit_active_step(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
    kind: EmailResourceType,
    actor: Uuid,
    at: DateTime<Utc>,
) -> KabiPayResult<usize> {
    let (row, entity) = resource(tx, tenant, id, kind).await?;
    let status: String = row.try_get("", "status")?;
    if status != "PENDING" {
        return Ok(0);
    }
    emit_workflow_actions(
        tx,
        None,
        WorkflowEmailActivation {
            transition: EmailTransition {
                tenant_id: tenant,
                resource_type: kind,
                resource_id: id,
                resource_version: email_timestamp_version(&status, row.try_get("", "updated_at")?),
                occurred_at: at,
                actor_ref: Some(EmailActorRef::LocalUser { user_id: actor }),
                actor_target_user_ids: vec![],
            },
            employee_id: row.try_get("", "employee_id")?,
            instance_id: row.try_get("", "workflow_instance_id")?,
            entity_type: entity,
            require_active_definition: false,
        },
    )
    .await
}
pub async fn emit_outcome(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    id: Uuid,
    kind: EmailResourceType,
    actor: Uuid,
    action: Uuid,
    at: DateTime<Utc>,
) -> KabiPayResult<usize> {
    let (row, _) = resource(tx, tenant, id, kind).await?;
    let status: String = row.try_get("", "status")?;
    if !matches!(
        status.as_str(),
        "APPROVED" | "PARTIAL_APPROVED" | "REJECTED"
    ) {
        return Err(KabiPayError::Internal(
            "approval outcome is not final".into(),
        ));
    }
    let key = workflow_email_outcome_key(&status, Some(action), at);
    emit_requester_outcome(
        tx,
        EmailTransition {
            tenant_id: tenant,
            resource_type: kind,
            resource_id: id,
            resource_version: key.clone(),
            occurred_at: at,
            actor_ref: Some(EmailActorRef::LocalUser { user_id: actor }),
                actor_target_user_ids: vec![],
        },
        row.try_get("", "employee_id")?,
        key,
    )
    .await
}
