use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use kabipay_organization::email_policy::verify_email_policy;
use kabipay_organization_security::{
    ServiceCredentialIssuer, ServiceCredentialVerifier, ServiceVerificationKey,
};
use uuid::Uuid;

#[test]
fn organization_mail_policy_accepts_only_the_owning_domain_and_exact_subject() {
    let now = Utc::now();
    let id = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let org = Uuid::new_v4();
    let membership = Uuid::new_v4();
    let event = EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: tenant,
        resource_type: EmailResourceType::Leave,
        resource_id: id,
        resource_version: "pending:1".into(),
        category: EmailCategory::Leave,
        kind: EmailEventKind::ActionRequired,
        template_version: 1,
        recipient: EmailRecipientRef::OrganizationPrincipal {
            principal_id: Uuid::new_v4(),
            source_tenant_id: Uuid::new_v4(),
            source_employee_id: Uuid::new_v4(),
        },
        actor_ref: None,
        authorization_ref: EmailAuthorizationRef::OrganizationGrant {
            organization_id: org,
            membership_id: membership,
            binding_id: Uuid::new_v4(),
            version: 1,
        },
        action_target: EmailActionTarget::OrganizationResource {
            organization_id: org,
            membership_id: membership,
            resource_type: EmailResourceType::Leave,
            resource_id: id,
        },
        occurred_at: now,
        expires_at: now + Duration::hours(1),
    };
    let request = EmailOrganizationPolicyRequest {
        intent: EmailEligibilityRequest {
            intent_id: Uuid::new_v4(),
            event_hash: event.canonical_hash().unwrap(),
            event,
        },
        subject_employee_id: Uuid::new_v4(),
    };
    let private =
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-private.pem");
    let public =
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-public.pem");
    for (service, allowed) in [
        ("kabipay-leave", true),
        ("kabipay-notification", false),
        ("kabipay-expense", false),
    ] {
        let issuer = ServiceCredentialIssuer::new(service, "mail-test", private).unwrap();
        let verifier = ServiceCredentialVerifier::new(
            EMAIL_ORGANIZATION_POLICY_AUDIENCE,
            vec![ServiceVerificationKey {
                kid: "mail-test".into(),
                service: service.into(),
                tenant_ids: vec![tenant],
                public_pem: public.to_vec(),
            }],
        )
        .unwrap();
        let body = serde_json::to_vec(&request).unwrap();
        let token = issuer
            .issue(
                EMAIL_ORGANIZATION_POLICY_AUDIENCE,
                tenant,
                EMAIL_ORGANIZATION_POLICY_ROUTE,
                &body,
            )
            .unwrap();
        assert_eq!(
            verify_email_policy(&verifier, &token, &body).is_ok(),
            allowed
        );
        let mut changed = request.clone();
        changed.subject_employee_id = Uuid::new_v4();
        assert!(
            verify_email_policy(&verifier, &token, &serde_json::to_vec(&changed).unwrap()).is_err()
        );
    }
}
