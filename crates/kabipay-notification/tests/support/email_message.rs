use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use uuid::Uuid;
pub fn event() -> EmailEvent {
    let tenant = Uuid::new_v4();
    let resource = Uuid::new_v4();
    let now = Utc::now();
    EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: tenant,
        resource_type: EmailResourceType::Test,
        resource_id: resource,
        resource_version: "settings:1".into(),
        category: EmailCategory::Test,
        kind: EmailEventKind::Test,
        template_version: 1,
        recipient: EmailRecipientRef::LocalUser {
            user_id: Uuid::new_v4(),
        },
        actor_ref: None,
        authorization_ref: EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Test,
            transition_key: "settings-test".into(),
        },
        action_target: EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Test,
            resource_id: resource,
        },
        occurred_at: now,
        expires_at: now + Duration::hours(1),
    }
}
