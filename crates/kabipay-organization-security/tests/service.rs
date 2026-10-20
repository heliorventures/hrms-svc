use kabipay_organization_security::*;
use uuid::Uuid;
const PRIVATE: &[u8] = include_bytes!("fixtures/test-a-private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test-a-public.pem");
fn verifier() -> ServiceCredentialVerifier {
    ServiceCredentialVerifier::new(
        "kabipay-organization",
        vec![ServiceVerificationKey {
            kid: "company-a".into(),
            service: "kabipay-leave".into(),
            tenant_ids: vec![Uuid::from_u128(1)],
            public_pem: PUBLIC.to_vec(),
        }],
    )
    .unwrap()
}
#[test]
fn service_proof_binds_company_audience_route_and_exact_body() {
    let issuer = ServiceCredentialIssuer::new("kabipay-leave", "company-a", PRIVATE).unwrap();
    let token = issuer
        .issue(
            "kabipay-organization",
            Uuid::from_u128(1),
            "/internal/organization/policy",
            b"request",
        )
        .unwrap();
    let v = verifier();
    let proof = v
        .verify(
            &token,
            Uuid::from_u128(1),
            "/internal/organization/policy",
            b"request",
        )
        .unwrap();
    assert_eq!(proof.claims().service, "kabipay-leave");
    assert!(v
        .verify(
            &token,
            Uuid::from_u128(2),
            "/internal/organization/policy",
            b"request"
        )
        .is_err());
    assert!(v
        .verify(
            &token,
            Uuid::from_u128(1),
            "/internal/organization/grants/ack",
            b"request"
        )
        .is_err());
    assert!(v
        .verify(
            &token,
            Uuid::from_u128(1),
            "/internal/organization/policy",
            b"changed"
        )
        .is_err());
    let cross = issuer
        .issue(
            "kabipay-organization",
            Uuid::from_u128(2),
            "/internal/organization/policy",
            b"request",
        )
        .unwrap();
    assert!(v
        .verify(
            &cross,
            Uuid::from_u128(2),
            "/internal/organization/policy",
            b"request"
        )
        .is_err());
    let wrong = issuer
        .issue(
            "kabipay-auth",
            Uuid::from_u128(1),
            "/internal/organization/policy",
            b"request",
        )
        .unwrap();
    assert!(v
        .verify(
            &wrong,
            Uuid::from_u128(1),
            "/internal/organization/policy",
            b"request"
        )
        .is_err());
}
#[test]
fn service_configuration_cannot_allow_wildcards_private_verification_keys_or_duplicate_kids() {
    assert!(ServiceCredentialVerifier::new("*", vec![]).is_err());
    for (service, tenants, pem) in [
        ("*", vec![Uuid::from_u128(1)], PUBLIC),
        ("kabipay-leave", vec![], PUBLIC),
        ("kabipay-leave", vec![Uuid::nil()], PUBLIC),
        ("kabipay-leave", vec![Uuid::from_u128(1)], PRIVATE),
    ] {
        assert!(ServiceCredentialVerifier::new(
            "kabipay-organization",
            vec![ServiceVerificationKey {
                kid: "key".into(),
                service: service.into(),
                tenant_ids: tenants,
                public_pem: pem.to_vec()
            }]
        )
        .is_err());
    }
}
#[test]
fn one_company_key_cannot_be_registered_for_another_company_or_service(){
    let a=Uuid::from_u128(1);let b=Uuid::from_u128(2);
    let key=|kid:&str,service:&str,tenant:Uuid|ServiceVerificationKey{kid:kid.into(),service:service.into(),tenant_ids:vec![tenant],public_pem:PUBLIC.to_vec()};
    assert!(ServiceCredentialVerifier::new("kabipay-organization",vec![key("a","kabipay-leave",a),key("b","kabipay-leave",b)]).is_err());
    assert!(ServiceCredentialVerifier::new("kabipay-organization",vec![key("a","kabipay-leave",a),key("b","kabipay-employee",a)]).is_err());
    let mut multiple=key("a","kabipay-leave",a);multiple.tenant_ids.push(b);
    assert!(ServiceCredentialVerifier::new("kabipay-organization",vec![multiple]).is_err());
}
