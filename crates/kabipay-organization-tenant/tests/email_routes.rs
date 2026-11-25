use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};
use axum::{
    body::Body,
    http::{Request, StatusCode},
};
use kabipay_common::{important_email::*, KabiPayResult};
use kabipay_organization_security::{
    ServiceCredentialIssuer, ServiceCredentialVerifier, ServiceVerificationKey,
};
use kabipay_organization_tenant::email_routes::{email_routes, EmailDomainAdapter};
use sea_orm::DatabaseConnection;
use std::sync::Arc;
use tower::ServiceExt;
#[path = "../../kabipay-common/tests/support/email_eligibility.rs"]
mod support;

struct Query;
#[Object]
impl Query {
    async fn ready(&self) -> bool {
        true
    }
}
#[derive(Clone)]
struct Domain;
impl EmailDomainAdapter for Domain {
    fn domain(&self) -> &'static str {
        "leave"
    }
    async fn check_email_eligibility(
        &self,
        _: &DatabaseConnection,
        _: &DatabaseConnection,
        _: &EmailEligibilityRequest,
    ) -> KabiPayResult<EmailEligibilityDecision> {
        panic!("unauthenticated request reached business policy")
    }
}

#[tokio::test]
async fn private_email_route_refuses_client_bearer_before_database_resolution() {
    let schema = Arc::new(Schema::build(Query, EmptyMutation, EmptySubscription).finish());
    let router = email_routes(Domain).with_state(schema);
    for bearer in [None, Some("Bearer client-jwt-is-not-service-authority")] {
        let mut request = Request::post(EMAIL_ELIGIBILITY_ROUTE);
        if let Some(value) = bearer {
            request = request.header("authorization", value);
        }
        let response = router
            .clone()
            .oneshot(request.body(Body::from("{}")).unwrap())
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    }
}

#[tokio::test]
async fn only_a_valid_notification_proof_can_reach_the_database_boundary() {
    use kabipay_organization_tenant::email_routes::email_routes_with_verifier;
    let request = support::request(EmailResourceType::Leave);
    let tenant = request.event.tenant_id;
    let verifier = ServiceCredentialVerifier::new(
        EMAIL_ELIGIBILITY_AUDIENCE,
        vec![ServiceVerificationKey {
            kid: "mail-test".into(),
            service: "kabipay-notification".into(),
            tenant_ids: vec![tenant],
            public_pem: include_bytes!(
                "../../kabipay-organization-security/tests/fixtures/test-a-public.pem"
            )
            .to_vec(),
        }],
    )
    .unwrap();
    let issuer = ServiceCredentialIssuer::new(
        "kabipay-notification",
        "mail-test",
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-private.pem"),
    )
    .unwrap();
    let body = serde_json::to_vec(&request).unwrap();
    let token = issuer
        .issue(
            EMAIL_ELIGIBILITY_AUDIENCE,
            tenant,
            EMAIL_ELIGIBILITY_ROUTE,
            &body,
        )
        .unwrap();
    let schema = Arc::new(Schema::build(Query, EmptyMutation, EmptySubscription).finish());
    let router = email_routes_with_verifier(Domain, Arc::new(verifier)).with_state(schema);
    for (credential, expected) in [
        (
            "client-jwt-is-not-service-authority",
            StatusCode::UNAUTHORIZED,
        ),
        (token.as_str(), StatusCode::SERVICE_UNAVAILABLE),
    ] {
        let response = router
            .clone()
            .oneshot(
                Request::post(EMAIL_ELIGIBILITY_ROUTE)
                    .header("authorization", format!("Bearer {credential}"))
                    .body(Body::from(body.clone()))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(
            response.status(),
            expected,
            "only authenticated requests reach the intentionally absent database context"
        );
    }
}

#[test]
fn mail_policy_destination_is_fixed_https_without_credentials_or_redirect_paths() {
    use kabipay_organization_tenant::email_policy::mail_policy_url;
    assert_eq!(
        mail_policy_url("https://organization.internal:8443")
            .unwrap()
            .path(),
        EMAIL_ORGANIZATION_POLICY_ROUTE
    );
    for invalid in [
        "http://127.0.0.1:8000",
        "https://user:password@organization.internal",
        "https://organization.internal/other",
        "https://organization.internal/?target=elsewhere",
        "https://organization.internal/#fragment",
        "file:///tmp/socket",
    ] {
        assert!(
            mail_policy_url(invalid).is_err(),
            "unexpected destination accepted"
        );
    }
}
