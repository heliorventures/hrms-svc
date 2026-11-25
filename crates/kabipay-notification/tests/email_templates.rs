#[path = "support/email_message.rs"]
mod support;
use kabipay_common::important_email::*;
use kabipay_notification::email::templates::{
    render_email, render_saved_account_test, EmailSender,
};
use uuid::Uuid;
fn sender() -> EmailSender {
    EmailSender::new(
        "Owning company".into(),
        "owner.sender@example.invalid".into(),
        "saved.account@example.invalid".into(),
    )
    .unwrap()
}
#[test]
fn saved_account_test_has_fixed_destination_and_stable_message_identity() {
    let event = support::event();
    let sender = sender();
    let a = render_saved_account_test(
        &event,
        "Synthetic company",
        "https://owner.example.invalid",
        &sender,
    )
    .unwrap();
    let b = render_saved_account_test(
        &event,
        "Synthetic company",
        "https://owner.example.invalid",
        &sender,
    )
    .unwrap();
    assert_eq!(a.recipient().to_string(), "saved.account@example.invalid");
    assert_eq!(a.bytes(), b.bytes());
    assert_eq!(a.message_id(), b.message_id());
    assert!(!format!("{a:?}").contains("saved.account@"));
}
#[test]
fn header_injection_and_untrusted_origins_are_rejected() {
    assert!(EmailSender::new(
        "Synthetic\r\nBcc: victim@example.invalid".into(),
        "owner@example.invalid".into(),
        "saved@example.invalid".into()
    )
    .is_err());
    assert!(EmailSender::new(
        "Synthetic".into(),
        "owner@example.invalid\r\n".into(),
        "saved@example.invalid".into()
    )
    .is_err());
    let event = support::event();
    for origin in [
        "http://owner.example.invalid",
        "https://user:pass@owner.example.invalid",
        "https://owner.example.invalid/redirect",
        "https://owner.example.invalid?next=evil",
    ] {
        assert!(render_saved_account_test(&event, "Synthetic company", origin, &sender()).is_err());
    }
    assert!(render_saved_account_test(
        &event,
        "Synthetic\nCompany",
        "https://owner.example.invalid",
        &sender()
    )
    .is_err());
}
#[test]
fn organization_mail_uses_verified_source_login_and_structured_owning_target_without_sensitive_values(
) {
    let mut event = support::event();
    let organization = Uuid::new_v4();
    let membership = Uuid::new_v4();
    let source = Uuid::new_v4();
    event.resource_type = EmailResourceType::Leave;
    event.kind = EmailEventKind::ActionRequired;
    event.category = EmailCategory::Leave;
    event.recipient = EmailRecipientRef::OrganizationPrincipal {
        principal_id: Uuid::new_v4(),
        source_tenant_id: source,
        source_employee_id: Uuid::new_v4(),
    };
    event.authorization_ref = EmailAuthorizationRef::OrganizationGrant {
        organization_id: organization,
        membership_id: membership,
        binding_id: Uuid::new_v4(),
        version: 1,
    };
    event.action_target = EmailActionTarget::OrganizationResource {
        organization_id: organization,
        membership_id: membership,
        resource_type: event.resource_type,
        resource_id: event.resource_id,
    };
    let recipient = PrivateEmailRecipient::new(
        EmailRecipientIdentity {
            source_tenant_id: source,
            source_user_id: Uuid::new_v4(),
        },
        "source.user@example.invalid".into(),
        "https://source.example.invalid".into(),
    )
    .unwrap();
    let rendered = render_email(
        &event,
        "Owning company",
        "https://owner.example.invalid",
        &sender(),
        &recipient,
    )
    .unwrap();
    assert!(rendered
        .link()
        .starts_with("https://source.example.invalid/organization-workspace?"));
    assert!(rendered
        .link()
        .contains(&format!("organization={organization}")));
    assert!(rendered.link().contains(&format!("company={membership}")));
    assert!(rendered.link().contains(&event.resource_id.to_string()));
    assert_eq!(
        rendered.sender().to_string(),
        "owner.sender@example.invalid"
    );
    assert!(!rendered.text().contains("https://owner.example.invalid"));
    for prohibited in ["medical", "salary", "bank", "document", "token="] {
        assert!(!rendered.text().contains(prohibited));
    }
}
#[test]
fn test_event_cannot_use_the_normal_arbitrary_recipient_entrypoint() {
    let event = support::event();
    let recipient = PrivateEmailRecipient::new(
        EmailRecipientIdentity {
            source_tenant_id: event.tenant_id,
            source_user_id: Uuid::new_v4(),
        },
        "unrelated@example.invalid".into(),
        "https://owner.example.invalid".into(),
    )
    .unwrap();
    assert!(render_email(
        &event,
        "Synthetic",
        "https://owner.example.invalid",
        &sender(),
        &recipient
    )
    .is_err());
}
#[test]
fn address_changes_keep_the_stable_user_message_id_and_local_identity_cannot_be_substituted() {
    let mut event = support::event();
    event.resource_type = EmailResourceType::Leave;
    event.category = EmailCategory::Leave;
    event.kind = EmailEventKind::Outcome;
    event.authorization_ref = EmailAuthorizationRef::DomainTransition {
        resource_type: event.resource_type,
        transition_key: "approved".into(),
    };
    event.action_target = EmailActionTarget::CompanyResource {
        resource_type: event.resource_type,
        resource_id: event.resource_id,
    };
    let EmailRecipientRef::LocalUser { user_id } = event.recipient else {
        unreachable!()
    };
    let identity = EmailRecipientIdentity {
        source_tenant_id: event.tenant_id,
        source_user_id: user_id,
    };
    let first = PrivateEmailRecipient::new(
        identity,
        "before@example.invalid".into(),
        "https://owner.example.invalid".into(),
    )
    .unwrap();
    let second = PrivateEmailRecipient::new(
        identity,
        "after@example.invalid".into(),
        "https://owner.example.invalid".into(),
    )
    .unwrap();
    let first = render_email(
        &event,
        "Synthetic",
        "https://owner.example.invalid",
        &sender(),
        &first,
    )
    .unwrap();
    let second = render_email(
        &event,
        "Synthetic",
        "https://owner.example.invalid",
        &sender(),
        &second,
    )
    .unwrap();
    assert_eq!(first.message_id(), second.message_id());
    let forged = PrivateEmailRecipient::new(
        EmailRecipientIdentity {
            source_tenant_id: event.tenant_id,
            source_user_id: Uuid::new_v4(),
        },
        "forged@example.invalid".into(),
        "https://owner.example.invalid".into(),
    )
    .unwrap();
    assert!(render_email(
        &event,
        "Synthetic",
        "https://owner.example.invalid",
        &sender(),
        &forged
    )
    .is_err());
}
