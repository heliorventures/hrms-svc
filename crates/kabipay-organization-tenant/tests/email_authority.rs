use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use kabipay_organization_security::{
    ServiceCredentialIssuer, ServiceCredentialVerifier, ServiceVerificationKey,
};
use kabipay_organization_tenant::email_transport::*;
use uuid::Uuid;

fn request() -> EmailEligibilityRequest {
    let now = Utc::now();
    let resource_id = Uuid::new_v4();
    let event = EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: Uuid::new_v4(),
        resource_type: EmailResourceType::Leave,
        resource_id,
        resource_version: "approved:1".into(),
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
            resource_id,
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

fn issuer(service: &str) -> ServiceCredentialIssuer {
    ServiceCredentialIssuer::new(
        service,
        "mail-test",
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-private.pem"),
    )
    .unwrap()
}
fn verifier(service: &str, tenant: Uuid, audience: &str) -> ServiceCredentialVerifier {
    ServiceCredentialVerifier::new(
        audience,
        vec![ServiceVerificationKey {
            kid: "mail-test".into(),
            service: service.into(),
            tenant_ids: vec![tenant],
            public_pem: include_bytes!(
                "../../kabipay-organization-security/tests/fixtures/test-a-public.pem"
            )
            .to_vec(),
        }],
    )
    .unwrap()
}

#[test]
fn body_or_audience_substitution_denied() {
    let request = request();
    let body = serde_json::to_vec(&request).unwrap();
    let token = issuer("kabipay-notification")
        .issue(
            EMAIL_ELIGIBILITY_AUDIENCE,
            request.event.tenant_id,
            EMAIL_ELIGIBILITY_ROUTE,
            &body,
        )
        .unwrap();
    let check = verifier(
        "kabipay-notification",
        request.event.tenant_id,
        EMAIL_ELIGIBILITY_AUDIENCE,
    );
    assert!(verify_eligibility_transport(&check, &token, &request, &body, "leave").is_ok());
    let mut changed = request.clone();
    changed.event.recipient = EmailRecipientRef::LocalUser {
        user_id: Uuid::new_v4(),
    };
    assert!(verify_eligibility_transport(
        &check,
        &token,
        &changed,
        &serde_json::to_vec(&changed).unwrap(),
        "leave"
    )
    .is_err());
    assert!(verify_eligibility_transport(&check, &token, &request, &body, "expense").is_err());
    let wrong = issuer("kabipay-notification")
        .issue(
            EMAIL_RECIPIENT_AUDIENCE,
            request.event.tenant_id,
            EMAIL_ELIGIBILITY_ROUTE,
            &body,
        )
        .unwrap();
    assert!(verify_eligibility_transport(&check, &wrong, &request, &body, "leave").is_err());
}

#[test]
fn mail_proof_cannot_execute_business_command() {
    let request = request();
    let body = serde_json::to_vec(&request).unwrap();
    let token = issuer("kabipay-leave")
        .issue(
            EMAIL_RECIPIENT_AUDIENCE,
            request.event.tenant_id,
            EMAIL_RECIPIENT_ROUTE,
            &body,
        )
        .unwrap();
    assert!(
        verifier("kabipay-leave", request.event.tenant_id, "kabipay-employee")
            .verify(
                &token,
                request.event.tenant_id,
                "/internal/organization/command",
                &body
            )
            .is_err()
    );
    assert!(verify_recipient_transport(
        &verifier(
            "kabipay-leave",
            request.event.tenant_id,
            EMAIL_RECIPIENT_AUDIENCE
        ),
        &token,
        &request,
        &body
    )
    .is_ok());
    // Notification is allowed to request eligibility, never to grant domain recipient lookup.
    let forged = issuer("kabipay-notification")
        .issue(
            EMAIL_RECIPIENT_AUDIENCE,
            request.event.tenant_id,
            EMAIL_RECIPIENT_ROUTE,
            &body,
        )
        .unwrap();
    assert!(verify_recipient_transport(
        &verifier(
            "kabipay-notification",
            request.event.tenant_id,
            EMAIL_RECIPIENT_AUDIENCE
        ),
        &forged,
        &request,
        &body
    )
    .is_err());
}

#[test]
fn full_event_digest_prevents_ack_only_identity_substitution() {
    let mut request = request();
    assert!(request.validate().is_ok());
    request.event.resource_version = "different-transition".into();
    assert!(request.validate().is_err());
}
