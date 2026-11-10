use kabipay_organization_security::*;
use uuid::Uuid;

const PRIVATE: &[u8] = include_bytes!("fixtures/test-a-private.pem");
const PUBLIC: &[u8] = include_bytes!("fixtures/test-a-public.pem");
const PUBLIC_B: &[u8] = include_bytes!("fixtures/test-b-public.pem");
const ROUTE: &str = "/internal/organization/policy";
fn key(kid: &str, service: &str, tenants: Vec<Uuid>, pem: &[u8]) -> ServiceVerificationKey {
    ServiceVerificationKey {
        kid: kid.into(),
        service: service.into(),
        tenant_ids: tenants,
        public_pem: pem.to_vec(),
    }
}
fn shared_key(tenants: Vec<Uuid>) -> ServiceVerificationKey {
    key("shared-leave-v1", "kabipay-leave", tenants, PUBLIC)
}

#[test]
fn shared_proof_accepts_only_explicit_companies_and_preserves_request_binding() {
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    let verifier = ServiceCredentialVerifier::new_with_shared_keys(
        "kabipay-organization",
        vec![shared_key(vec![a, b])],
        &["shared-leave-v1"],
    )
    .unwrap();
    let issuer = ServiceCredentialIssuer::new("kabipay-leave", "shared-leave-v1", PRIVATE).unwrap();
    for tenant in [a, b] {
        let token = issuer
            .issue("kabipay-organization", tenant, ROUTE, b"request")
            .unwrap();
        assert_eq!(
            verifier
                .verify(&token, tenant, ROUTE, b"request")
                .unwrap()
                .claims()
                .tenant_id,
            tenant
        );
        assert!(verifier
            .verify(&token, if tenant == a { b } else { a }, ROUTE, b"request")
            .is_err());
        assert!(verifier.verify(&token, tenant, ROUTE, b"changed").is_err());
        assert!(verifier
            .verify(&token, tenant, "/internal/organization/other", b"request")
            .is_err());
    }
    let unlisted = Uuid::from_u128(3);
    let token = issuer
        .issue("kabipay-organization", unlisted, ROUTE, b"request")
        .unwrap();
    assert!(verifier
        .verify(&token, unlisted, ROUTE, b"request")
        .is_err());
    let token = issuer.issue("kabipay-auth", a, ROUTE, b"request").unwrap();
    assert!(verifier.verify(&token, a, ROUTE, b"request").is_err());
    let other =
        ServiceCredentialIssuer::new("kabipay-employee", "shared-leave-v1", PRIVATE).unwrap();
    let token = other
        .issue("kabipay-organization", a, ROUTE, b"request")
        .unwrap();
    assert!(verifier.verify(&token, a, ROUTE, b"request").is_err());
}

#[test]
fn shared_opt_in_is_key_specific_and_isolated_default_is_preserved() {
    let tenants = vec![Uuid::from_u128(1), Uuid::from_u128(2)];
    assert!(ServiceCredentialVerifier::new(
        "kabipay-organization",
        vec![shared_key(tenants.clone())]
    )
    .is_err());
    for selectors in [
        vec!["unknown"],
        vec!["shared-leave-v1", "shared-leave-v1"],
        vec!["*"],
    ] {
        assert!(ServiceCredentialVerifier::new_with_shared_keys(
            "kabipay-organization",
            vec![shared_key(tenants.clone())],
            &selectors
        )
        .is_err());
    }
    let unrelated = key(
        "bound-employee-v1",
        "kabipay-employee",
        tenants.clone(),
        PUBLIC_B,
    );
    assert!(ServiceCredentialVerifier::new_with_shared_keys(
        "kabipay-organization",
        vec![shared_key(tenants), unrelated],
        &["shared-leave-v1"]
    )
    .is_err());
}

#[test]
fn shared_scope_rejects_empty_nil_duplicate_and_oversized_company_sets() {
    for tenants in [
        vec![],
        vec![Uuid::nil()],
        vec![Uuid::from_u128(1); 2],
        (1..=101).map(Uuid::from_u128).collect(),
    ] {
        assert!(ServiceCredentialVerifier::new_with_shared_keys(
            "kabipay-organization",
            vec![shared_key(tenants)],
            &["shared-leave-v1"]
        )
        .is_err());
    }
    assert!(ServiceCredentialVerifier::new_with_shared_keys(
        "kabipay-organization",
        vec![shared_key((1..=100).map(Uuid::from_u128).collect())],
        &["shared-leave-v1"]
    )
    .is_ok());
}

#[test]
fn shared_mode_rejects_duplicate_ids_and_reused_material_for_different_scopes_or_services() {
    let a = Uuid::from_u128(1);
    let b = Uuid::from_u128(2);
    for other in [
        key("alias", "kabipay-leave", vec![a], PUBLIC),
        key("alias", "kabipay-employee", vec![a, b], PUBLIC),
        key("shared-leave-v1", "kabipay-leave", vec![a, b], PUBLIC_B),
    ] {
        assert!(ServiceCredentialVerifier::new_with_shared_keys(
            "kabipay-organization",
            vec![shared_key(vec![a, b]), other],
            &["shared-leave-v1", "alias"]
        )
        .is_err());
    }
}
