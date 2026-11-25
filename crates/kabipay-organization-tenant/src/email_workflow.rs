//! Reuses the action's existing workflow/scope rule. This never executes it.
use crate::{email_policy::current_email_organization_policy, statement};
use kabipay_common::{context::ScopeType, important_email::*, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseConnection};
use uuid::Uuid;

pub struct WorkflowEmailResource {
    pub instance_id: Option<Uuid>,
    pub entity_type: &'static str,
    /// Loans require an active definition at decision time; preserve each
    /// existing domain's rule rather than changing business policy for email.
    pub require_active_definition: bool,
}

pub async fn check_workflow_email(
    db: &DatabaseConnection,
    request: &EmailEligibilityRequest,
    mut subject: EmailSubjectSnapshot,
    resource: WorkflowEmailResource,
) -> KabiPayResult<EmailEligibilityDecision> {
    request.validate()?;
    let event = &request.event;
    if event.kind == EmailEventKind::Outcome {
        return Ok(match check_email_subject(event, &subject) {
            EmailSubjectCheck::RequesterOutcome => EmailEligibilityDecision::Eligible,
            EmailSubjectCheck::Obsolete => EmailEligibilityDecision::Obsolete,
            _ => EmailEligibilityDecision::Forbidden,
        });
    }
    if event.kind != EmailEventKind::ActionRequired {
        return Ok(EmailEligibilityDecision::Forbidden);
    }
    if !subject.pending {
        return Ok(EmailEligibilityDecision::Obsolete);
    }
    let Some(instance) = resource.instance_id else {
        return Ok(EmailEligibilityDecision::Obsolete);
    };
    let Some(workflow) = load_email_workflow(
        db,
        event.tenant_id,
        event.resource_id,
        resource.entity_type,
        instance,
        resource.require_active_definition,
    )
    .await?
    else {
        return Ok(EmailEligibilityDecision::Obsolete);
    };
    subject.resource_version = workflow.version(&subject.resource_version);
    match check_email_subject(event, &subject) {
        EmailSubjectCheck::Obsolete => return Ok(EmailEligibilityDecision::Obsolete),
        EmailSubjectCheck::CheckCurrentActionAuthority => {}
        _ => return Ok(EmailEligibilityDecision::Forbidden),
    }
    let permission =
        email_action_permission(event.resource_type).ok_or(KabiPayError::Unauthorised)?;
    let result = match event.recipient {
        EmailRecipientRef::LocalUser { user_id } => {
            if !matches!(event.authorization_ref,EmailAuthorizationRef::LocalWorkflow{instance_id,step_id} if instance_id==workflow.instance_id && step_id==workflow.step_id)
            {
                return Ok(EmailEligibilityDecision::Obsolete);
            }
            let Some(scope) =
                current_email_permission_scope(db, event.tenant_id, user_id, permission).await?
            else {
                return Ok(EmailEligibilityDecision::Forbidden);
            };
            if event.resource_type != EmailResourceType::LoanRequest
                && ![ScopeType::Team, ScopeType::All].contains(&scope)
            {
                return Ok(EmailEligibilityDecision::Forbidden);
            }
            workflow
                .local_actor_allows(
                    db,
                    event.tenant_id,
                    subject.employee_id,
                    user_id,
                    scope,
                    permission,
                )
                .await
        }
        EmailRecipientRef::OrganizationPrincipal { principal_id, .. } => {
            let policy = current_email_organization_policy(&EmailOrganizationPolicyRequest {
                intent: request.clone(),
                subject_employee_id: subject.employee_id,
            })
            .await?;
            require_email_installation(
                db,
                event,
                subject.employee_id,
                principal_id,
                permission,
                &policy,
            )
            .await?;
            let manager = if policy.reporting_manager
                && workflow.relationship_allows(permission, false).is_err()
            {
                crate::transport::workflow_reporting_manager_current(
                    db,
                    event.tenant_id,
                    subject.employee_id,
                    principal_id,
                    match event.action_target {
                        EmailActionTarget::OrganizationResource {
                            organization_id, ..
                        } => organization_id,
                        _ => return Ok(EmailEligibilityDecision::Forbidden),
                    },
                    permission,
                    false,
                )
                .await?
            } else {
                false
            };
            workflow.relationship_allows(permission, manager)
        }
    };
    action_decision(result)
}

pub fn action_decision(result: KabiPayResult<()>) -> KabiPayResult<EmailEligibilityDecision> {
    match result {
        Ok(()) => Ok(EmailEligibilityDecision::Eligible),
        Err(
            KabiPayError::Unauthorised
            | KabiPayError::Forbidden(_)
            | KabiPayError::Validation(_)
            | KabiPayError::NotFound { .. },
        ) => Ok(EmailEligibilityDecision::Forbidden),
        Err(error) => Err(error),
    }
}

/// Source installation must agree with live metadata before a mail proof is
/// issued. Reads take no business row locks and confer no update authority.
pub async fn require_email_installation(
    db: &impl ConnectionTrait,
    event: &EmailEvent,
    subject: Uuid,
    principal: Uuid,
    permission: &str,
    policy: &EmailOrganizationPolicy,
) -> KabiPayResult<()> {
    let denied = || KabiPayError::Unauthorised;
    let (org, member, authority, version, relationship) = match event.authorization_ref {
        EmailAuthorizationRef::OrganizationGrant {
            organization_id,
            membership_id,
            binding_id,
            version,
        } => (organization_id, membership_id, binding_id, version, false),
        EmailAuthorizationRef::OrganizationRelationship {
            organization_id,
            membership_id,
            relationship_id,
            version,
        } => (
            organization_id,
            membership_id,
            relationship_id,
            version,
            true,
        ),
        _ => return Err(denied()),
    };
    if db.query_one(statement("SELECT membership_id FROM organization_membership_installation WHERE tenant_id=$1 AND organization_id=$2 AND membership_id=$3 AND version=$4 AND status='ACTIVE'",vec![event.tenant_id.into(),org.into(),member.into(),policy.membership_version.into()])).await?.is_none(){return Err(denied());}
    if db.query_one(statement("SELECT id FROM management_assignment WHERE tenant_id=$1 AND subject_employee_id=$2 AND subject_principal_id=$3",vec![event.tenant_id.into(),subject.into(),principal.into()])).await?.is_some(){return Err(denied());}
    let present = if relationship {
        db.query_one(statement("SELECT a.id FROM management_assignment a JOIN assignment_capability c ON c.assignment_id=a.id AND c.tenant_id=a.tenant_id WHERE a.tenant_id=$1 AND a.id=$2 AND a.organization_id=$3 AND a.membership_id=$4 AND a.manager_principal_id=$5 AND a.subject_employee_id=$6 AND a.subject_principal_id<>a.manager_principal_id AND a.version=$7 AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND c.capability_code=$8",vec![event.tenant_id.into(),authority.into(),org.into(),member.into(),principal.into(),subject.into(),version.into(),permission.into()])).await?.is_some()
    } else {
        db.query_one(statement("SELECT binding_id FROM organization_grant_installation WHERE tenant_id=$1 AND binding_id=$2 AND organization_id=$3 AND membership_id=$4 AND principal_id=$5 AND grant_version=$6 AND membership_version=$7 AND status='ACTIVE' AND capabilities @> $8::jsonb",vec![event.tenant_id.into(),authority.into(),org.into(),member.into(),principal.into(),version.into(),policy.membership_version.into(),serde_json::json!([{"code":permission,"employee_scope":policy.scope}]).into()])).await?.is_some()
    };
    if !present {
        return Err(denied());
    }
    Ok(())
}
