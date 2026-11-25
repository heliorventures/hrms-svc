//! Exact pending workflow revision, shared by selection and dispatch checks.
use super::{current_email_actor, email_workflow_version};
use crate::{
    context::ScopeType,
    workflow_approval::{
        assert_workflow_step_actor_read_only, assert_workflow_step_relationship_for_permission,
        WorkflowApprovalAuthority,
    },
    KabiPayError, KabiPayResult,
};
use chrono::{DateTime, Utc};
use kabipay_db_entities::tenant::d0025_workflow::workflow_step;
use sea_orm::{ConnectionTrait, DbBackend, FromQueryResult, Statement};
use uuid::Uuid;

pub struct EmailWorkflowSnapshot {
    pub instance_id: Uuid,
    pub step_id: Uuid,
    revision: DateTime<Utc>,
    step: workflow_step::Model,
}
impl EmailWorkflowSnapshot {
    pub fn local_reviewer_allows(
        &self,
        reviewer: &super::LocalEmailReviewer,
        permission: &str,
        allowed_scopes: &[ScopeType],
    ) -> bool {
        allowed_scopes.contains(&reviewer.scope)
            && self
                .relationship_allows(permission, reviewer.is_reporting_manager)
                .is_ok()
    }
    pub fn version(&self, domain_version: &str) -> String {
        email_workflow_version(
            domain_version,
            self.instance_id,
            self.step_id,
            self.revision,
        )
    }
    pub fn relationship_allows(
        &self,
        permission: &str,
        reporting_manager: bool,
    ) -> KabiPayResult<()> {
        assert_workflow_step_relationship_for_permission(
            &self.step,
            permission,
            if reporting_manager {
                Ok(())
            } else {
                Err(KabiPayError::Unauthorised)
            },
        )
    }
    pub async fn local_actor_allows(
        &self,
        db: &(impl ConnectionTrait + Sync),
        tenant: Uuid,
        subject: Uuid,
        user: Uuid,
        scope: ScopeType,
        permission: &'static str,
    ) -> KabiPayResult<()> {
        let actor = current_email_actor(db, tenant, user)
            .await?
            .ok_or(KabiPayError::Unauthorised)?;
        assert_workflow_step_actor_read_only(
            db,
            tenant,
            subject,
            &self.step,
            &WorkflowApprovalAuthority {
                actor_user_id: user,
                actor_employee: Some(actor),
                scope,
                permission,
            },
        )
        .await
    }
}

pub const EMAIL_CURRENT_WORKFLOW_SQL: &str = r#"
SELECT s.id,s.tenant_id,s.workflow_id,s.sequence_order,s.step_name,s.approver_type,s.approver_role_id,s.approver_permission,
 s.can_skip,s.sla_hours,s.created_at,s.updated_at,i.updated_at AS instance_updated_at
FROM workflow_instance i JOIN workflow w ON w.id=i.workflow_id AND w.tenant_id=i.tenant_id AND w.entity_type=i.entity_type
JOIN workflow_step s ON s.id=i.current_step_id AND s.tenant_id=i.tenant_id AND s.workflow_id=i.workflow_id
WHERE i.tenant_id=$1 AND i.id=$2 AND i.entity_id=$3 AND i.entity_type=$4 AND i.status='IN_PROGRESS'
 AND (NOT $5::boolean OR w.is_active)
"#;

pub async fn load_email_workflow(
    db: &impl ConnectionTrait,
    tenant: Uuid,
    resource: Uuid,
    entity_type: &str,
    instance: Uuid,
    require_active_definition: bool,
) -> KabiPayResult<Option<EmailWorkflowSnapshot>> {
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            EMAIL_CURRENT_WORKFLOW_SQL,
            [
                tenant.into(),
                instance.into(),
                resource.into(),
                entity_type.into(),
                require_active_definition.into(),
            ],
        ))
        .await?;
    row.map(|row| {
        let step = workflow_step::Model::from_query_result(&row, "")?;
        Ok(EmailWorkflowSnapshot {
            instance_id: instance,
            step_id: step.id,
            revision: row.try_get("", "instance_updated_at")?,
            step,
        })
    })
    .transpose()
}

pub fn workflow_email_outcome_key(status: &str, action: Option<Uuid>, at: DateTime<Utc>) -> String {
    match action {
        Some(id) => format!("{}:action:{id}", status.to_ascii_lowercase()),
        None => format!(
            "{}:at:{}",
            status.to_ascii_lowercase(),
            at.timestamp_micros()
        ),
    }
}
pub const WORKFLOW_EMAIL_OUTCOME_SQL: &str = r#"
SELECT a.id FROM workflow_action a JOIN workflow_instance i ON i.tenant_id=a.tenant_id AND i.id=a.instance_id
WHERE a.tenant_id=$1 AND a.id=$2 AND a.instance_id=$3 AND a.action=$4 AND a.acted_at=i.completed_at
 AND i.entity_id=$5 AND i.entity_type=$6 AND i.current_step_id IS NULL
 AND i.status IN ('COMPLETED','REJECTED','CANCELLED')
"#;
/// A meaningful final decision binds its immutable action receipt, so later
/// payment metadata does not erase it. Cancellation/reversal suppresses the old
/// outcome. Intermediate approvals activate the next step, not an outcome.
pub async fn workflow_email_outcome_is_current(
    db: &impl ConnectionTrait,
    event: &super::EmailEvent,
    instance: Option<Uuid>,
    entity_type: &str,
    status: &str,
    updated_at: DateTime<Utc>,
) -> KabiPayResult<bool> {
    use super::{EmailAuthorizationRef, EmailEventKind};
    if event.kind != EmailEventKind::Outcome {
        return Ok(false);
    }
    let EmailAuthorizationRef::DomainTransition {
        resource_type,
        transition_key,
    } = &event.authorization_ref
    else {
        return Ok(false);
    };
    if *resource_type != event.resource_type || *transition_key != event.resource_version {
        return Ok(false);
    }
    if status == "CANCELLED" {
        return Ok(
            *transition_key == workflow_email_outcome_key(status, None, updated_at)
                && event.occurred_at.timestamp_micros() == updated_at.timestamp_micros(),
        );
    }
    let action = match status {
        "APPROVED" | "PARTIAL_APPROVED" => "APPROVE",
        "REJECTED" => "REJECT",
        _ => return Ok(false),
    };
    let prefix = format!("{}:action:", status.to_ascii_lowercase());
    let Some(id) = transition_key
        .strip_prefix(&prefix)
        .and_then(|s| Uuid::parse_str(s).ok())
    else {
        return Ok(false);
    };
    let Some(instance) = instance else {
        return Ok(false);
    };
    Ok(db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            WORKFLOW_EMAIL_OUTCOME_SQL,
            [
                event.tenant_id.into(),
                id.into(),
                instance.into(),
                action.into(),
                event.resource_id.into(),
                entity_type.into(),
            ],
        ))
        .await?
        .is_some())
}
