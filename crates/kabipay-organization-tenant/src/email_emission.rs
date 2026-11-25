//! Transaction-owned workflow producers. Selection reads identity/authority
//! metadata only; SMTP, addresses, keys and configuration never enter here.
use crate::{email_reviewers::select_organization_email_reviewers, statement};
use kabipay_common::{context::ScopeType, important_email::*, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;

pub struct WorkflowEmailActivation {
    pub transition: EmailTransition,
    pub employee_id: Uuid,
    pub instance_id: Option<Uuid>,
    pub entity_type: &'static str,
    pub require_active_definition: bool,
}
pub async fn emit_workflow_actions(
    tx: &DatabaseTransaction,
    metadata: Option<&DatabaseConnection>,
    activation: WorkflowEmailActivation,
) -> KabiPayResult<usize> {
    let WorkflowEmailActivation {
        mut transition,
        employee_id,
        instance_id,
        entity_type,
        require_active_definition,
    } = activation;
    let Some(instance) = instance_id else {
        return Ok(0);
    };
    let Some(workflow) = load_email_workflow(
        tx,
        transition.tenant_id,
        transition.resource_id,
        entity_type,
        instance,
        require_active_definition,
    )
    .await?
    else {
        return Ok(0);
    };
    transition.resource_version = workflow.version(&transition.resource_version);
    let permission =
        email_action_permission(transition.resource_type).ok_or(KabiPayError::Unauthorised)?;
    let scopes = if transition.resource_type == EmailResourceType::LoanRequest {
        &[ScopeType::All, ScopeType::Team, ScopeType::Department][..]
    } else {
        &[ScopeType::All, ScopeType::Team][..]
    };
    let mut reviewers = Vec::new();
    for actor in
        select_local_email_reviewers(tx, transition.tenant_id, employee_id, permission).await?
    {
        if workflow.local_reviewer_allows(&actor, permission, scopes) {
            reviewers.push(EmailReviewer {
                recipient: EmailRecipientRef::LocalUser {
                    user_id: actor.user_id,
                },
                authorization_ref: EmailAuthorizationRef::LocalWorkflow {
                    instance_id: workflow.instance_id,
                    step_id: workflow.step_id,
                },
                action_target: EmailActionTarget::CompanyResource {
                    resource_type: transition.resource_type,
                    resource_id: transition.resource_id,
                },
            });
        }
    }
    if matches!(
        transition.resource_type,
        EmailResourceType::Leave | EmailResourceType::Timesheet
    ) {
        let metadata = metadata.ok_or_else(|| {
            KabiPayError::Internal(
                "trusted metadata connection is required for workflow recipient selection".into(),
            )
        })?;
        for actor in select_organization_email_reviewers(
            tx,
            metadata,
            transition.tenant_id,
            employee_id,
            transition.resource_type,
            transition.resource_id,
        )
        .await?
        {
            if workflow
                .relationship_allows(permission, actor.is_reporting_manager)
                .is_err()
            {
                continue;
            }
            if let (
                Some(EmailActorRef::LocalUser { user_id }),
                EmailRecipientRef::OrganizationPrincipal {
                    source_tenant_id, ..
                },
            ) = (&transition.actor_ref, &actor.reviewer.recipient)
            {
                if *source_tenant_id == transition.tenant_id && *user_id == actor.source_user_id {
                    continue;
                }
            }
            reviewers.push(actor.reviewer);
        }
    }
    append_transition_emails(tx, &transition, EmailEventKind::ActionRequired, reviewers).await
}

/// The requester remains an identity reference; Employee resolves its current
/// mailbox at dispatch. A missing/ambiguous/inactive link produces no new alert.
pub async fn emit_requester_outcome(
    tx: &DatabaseTransaction,
    transition: EmailTransition,
    employee: Uuid,
    transition_key: String,
) -> KabiPayResult<usize> {
    emit_employee_event(tx,transition,employee,transition_key,EmailEventKind::Outcome).await
}
pub async fn emit_employee_event(
    tx: &DatabaseTransaction, transition: EmailTransition, employee: Uuid,
    transition_key: String, kind: EmailEventKind,
) -> KabiPayResult<usize> {
    let rows=tx.query_all(statement("SELECT e.user_id FROM employee e JOIN \"user\" u ON u.tenant_id=e.tenant_id AND u.id=e.user_id WHERE e.tenant_id=$1 AND e.id=$2 AND NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE') AND u.is_active AND NOT u.is_deleted",vec![transition.tenant_id.into(),employee.into()])).await?;
    if rows.len() != 1 {
        return Ok(0);
    }
    let user_id: Uuid = rows[0].try_get("", "user_id")?;
    let reviewer = EmailReviewer {
        recipient: EmailRecipientRef::LocalUser { user_id },
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: transition.resource_type,
            transition_key,
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: transition.resource_type,
            resource_id: transition.resource_id,
        },
    };
    append_transition_emails(tx, &transition, kind, vec![reviewer]).await
}
