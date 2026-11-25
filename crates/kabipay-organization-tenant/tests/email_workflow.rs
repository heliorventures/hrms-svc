use chrono::{Duration, Utc};
use kabipay_common::{important_email::*, KabiPayResult};
use kabipay_organization_tenant::email_workflow::{check_workflow_email, WorkflowEmailResource};
use sea_orm::DatabaseConnection;
use uuid::Uuid;

fn request() -> EmailEligibilityRequest {
    let now = Utc::now();
    let resource = Uuid::new_v4();
    let event = EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        resource_type: EmailResourceType::Leave,
        resource_id: resource,
        resource_version: "APPROVED:1".into(),
        category: EmailCategory::Leave,
        kind: EmailEventKind::Outcome,
        template_version: 1,
        recipient: EmailRecipientRef::LocalUser {
            user_id: Uuid::new_v4(),
        },
        actor_ref: None,
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Leave,
            transition_key: "approved:1".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Leave,
            resource_id: resource,
        },
        occurred_at: now,
        expires_at: now + Duration::days(7),
    };
    EmailEligibilityRequest {
        intent_id: Uuid::new_v4(),
        event_hash: event.canonical_hash().unwrap(),
        event,
    }
}
#[tokio::test]
async fn requester_outcome_survives_finalization_without_loading_workflow_authority(
) -> KabiPayResult<()> {
    let request = request();
    let user = match request.event.recipient {
        EmailRecipientRef::LocalUser { user_id } => user_id,
        _ => unreachable!(),
    };
    let subject = EmailSubjectSnapshot {
        employee_id: Uuid::new_v4(),
        requester_user_id: Some(user),
        resource_version: "finalized-newer-revision".into(),
        pending: false,
        outcome_is_current: true,
    };
    let workflow = WorkflowEmailResource {
        instance_id: None,
        entity_type: "LEAVE_REQUEST",
        require_active_definition: false,
    };
    assert_eq!(
        check_workflow_email(
            &DatabaseConnection::Disconnected,
            &request,
            subject,
            workflow
        )
        .await?,
        EmailEligibilityDecision::Eligible
    );
    Ok(())
}
#[tokio::test]
async fn resolved_action_is_obsolete_without_trying_an_identity_lookup() -> KabiPayResult<()> {
    let mut request = request();
    request.event.kind = EmailEventKind::ActionRequired;
    request.event.expires_at = request.event.occurred_at + Duration::hours(1);
    request.event_hash = request.event.canonical_hash()?;
    let subject = EmailSubjectSnapshot {
        employee_id: Uuid::new_v4(),
        requester_user_id: None,
        resource_version: request.event.resource_version.clone(),
        pending: false,
        outcome_is_current: true,
    };
    let workflow = WorkflowEmailResource {
        instance_id: None,
        entity_type: "LEAVE_REQUEST",
        require_active_definition: false,
    };
    assert_eq!(
        check_workflow_email(
            &DatabaseConnection::Disconnected,
            &request,
            subject,
            workflow
        )
        .await?,
        EmailEligibilityDecision::Obsolete
    );
    Ok(())
}
