use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use uuid::Uuid;
#[test]
fn event_factory_keeps_committed_identity_and_never_notifies_the_actor() {
    let actor = EmailRecipientRef::LocalUser {
        user_id: Uuid::new_v4(),
    };
    let now = Utc::now();
    let transition = EmailTransition {
        tenant_id: Uuid::new_v4(),
        resource_type: EmailResourceType::Leave,
        resource_id: Uuid::new_v4(),
        resource_version: "approved:receipt".into(),
        occurred_at: now,
        actor_ref: Some(actor.clone().into()),
        actor_target_user_ids: vec![],
    };
    let reviewer = EmailReviewer {
        recipient: actor,
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Leave,
            transition_key: "approved".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Leave,
            resource_id: transition.resource_id,
        },
    };
    assert!(transition
        .event(EmailEventKind::Outcome, reviewer)
        .unwrap()
        .is_none());
    let make = || EmailReviewer {
        recipient: EmailRecipientRef::LocalUser {
            user_id: Uuid::from_u128(100),
        },
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Leave,
            transition_key: "approved".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Leave,
            resource_id: transition.resource_id,
        },
    };
    let first = transition
        .event(EmailEventKind::Outcome, make())
        .unwrap()
        .unwrap();
    let second = transition
        .event(EmailEventKind::Outcome, make())
        .unwrap()
        .unwrap();
    assert_eq!(first, second);
    assert_eq!(first.occurred_at, now);
    assert_eq!(first.expires_at, now + Duration::days(7));
    assert_eq!(first.category, EmailCategory::Leave);
    let action = transition
        .event(EmailEventKind::ActionRequired, make())
        .unwrap()
        .unwrap();
    assert_eq!(action.expires_at, now + Duration::hours(24));
    assert_ne!(first.event_id, action.event_id);
}
#[test]
fn foreign_actor_identity_does_not_require_inventing_a_source_employment() {
    let principal = Uuid::new_v4();
    let actor = EmailActorRef::OrganizationPrincipal {
        principal_id: principal,
    };
    let recipient = EmailRecipientRef::OrganizationPrincipal {
        principal_id: principal,
        source_tenant_id: Uuid::new_v4(),
        source_employee_id: Uuid::new_v4(),
    };
    assert!(same_email_actor(&actor, &recipient));
    assert!(!serde_json::to_string(&actor)
        .unwrap()
        .contains("source_employee_id"));
}

#[test]
fn foreign_actor_local_login_alias_is_suppressed_without_persisting_aliases() {
    let local = Uuid::new_v4();
    let transition = EmailTransition {
        tenant_id: Uuid::new_v4(),
        resource_type: EmailResourceType::Leave,
        resource_id: Uuid::new_v4(),
        resource_version: "next:step".into(),
        occurred_at: Utc::now(),
        actor_ref: Some(EmailActorRef::OrganizationPrincipal {
            principal_id: Uuid::new_v4(),
        }),
        actor_target_user_ids: vec![local],
    };
    let reviewer = |user_id| EmailReviewer {
        recipient: EmailRecipientRef::LocalUser { user_id },
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Leave,
            transition_key: "next:step".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Leave,
            resource_id: transition.resource_id,
        },
    };
    assert!(transition
        .event(EmailEventKind::ActionRequired, reviewer(local))
        .unwrap()
        .is_none());
    let event = transition
        .event(EmailEventKind::ActionRequired, reviewer(Uuid::new_v4()))
        .unwrap()
        .unwrap();
    assert!(!serde_json::to_string(&event)
        .unwrap()
        .contains(&local.to_string()));
}

#[test]
fn availability_notice_is_not_an_approval_self_alert() {
    let user = Uuid::new_v4();
    let transition = EmailTransition {
        tenant_id: Uuid::new_v4(),
        resource_type: EmailResourceType::Payslip,
        resource_id: Uuid::new_v4(),
        resource_version: "GENERATED:1".into(),
        occurred_at: Utc::now(),
        actor_ref: Some(EmailActorRef::LocalUser { user_id: user }),
        actor_target_user_ids: vec![user],
    };
    let reviewer = local_transition_reviewer(&transition, user, "generated");
    assert!(transition
        .event(EmailEventKind::PayslipAvailable, reviewer)
        .unwrap()
        .is_some());
}
