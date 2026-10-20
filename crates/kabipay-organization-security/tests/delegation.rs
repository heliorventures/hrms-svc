use chrono::Utc;
use jsonwebtoken::{encode, Algorithm, EncodingKey, Header};
use kabipay_organization_domain::*;
use kabipay_organization_security::*;
use uuid::Uuid;

const PRIVATE_A: &[u8] = include_bytes!("fixtures/test-a-private.pem");
const PUBLIC_A: &[u8] = include_bytes!("fixtures/test-a-public.pem");
const PRIVATE_B: &[u8] = include_bytes!("fixtures/test-b-private.pem");
const PUBLIC_B: &[u8] = include_bytes!("fixtures/test-b-public.pem");
fn id(value: u128) -> Uuid {
    Uuid::from_u128(value)
}
fn version(value: u64) -> ResourceVersion {
    ResourceVersion::try_from(value).unwrap()
}
fn claims() -> DelegationClaims {
    let now = Utc::now().timestamp() as u64;
    DelegationClaims {
        iss: DELEGATION_ISSUER.into(),
        aud: "kabipay-leave".into(),
        sub: PrincipalId::try_from(id(1)).unwrap(),
        jti: id(2),
        iat: now,
        nbf: now,
        exp: now + 120,
        organization_id: OrganizationId::try_from(id(3)).unwrap(),
        target_membership_id: MembershipId::try_from(id(4)).unwrap(),
        source_tenant_id: TenantId::try_from(id(5)).unwrap(),
        source_user_id: id(6),
        target_tenant_id: TenantId::try_from(id(7)).unwrap(),
        identity_version: version(1),
        source_link_version: version(1),
        target_membership_version: version(1),
        authority_kind: AuthorityKind::ManagementAssignment,
        authority_ref: id(8),
        authority_version: version(2),
        capability: Capability::parse("leave:approve").unwrap(),
        resource_type: "leave_request".into(),
        resource_id: id(9),
        expected_resource_version: version(3),
        command_id: id(10),
        request_hash: command_body_hash(b"approve"),
    }
}
fn expected(c: &DelegationClaims) -> CommandExpectation<'_> {
    CommandExpectation {
        capability: &c.capability,
        resource_type: &c.resource_type,
        resource_id: c.resource_id,
        expected_resource_version: c.expected_resource_version,
        command_id: c.command_id,
        request_hash: &c.request_hash,
    }
}
fn verifier() -> DelegationVerifier {
    DelegationVerifier::new(
        TenantId::try_from(id(7)).unwrap(),
        "kabipay-leave",
        vec![("a", PUBLIC_A)],
    )
    .unwrap()
}
fn token(c: &DelegationClaims) -> String {
    let mut h = Header::new(Algorithm::RS256);
    h.kid = Some("a".into());
    h.typ = Some(DELEGATION_TOKEN_TYPE.into());
    encode(&h, c, &EncodingKey::from_rsa_pem(PRIVATE_A).unwrap()).unwrap()
}

#[test]
fn valid_delegation_preserves_original_actor_and_command_without_creating_an_employee() {
    let c = claims();
    let issuer = DelegationIssuer::new("a", PRIVATE_A).unwrap();
    let encoded = issuer.issue(&c).unwrap();
    let verified = verifier().verify(&encoded, &expected(&c)).unwrap();
    assert_eq!(verified.claims().sub, c.sub);
    assert_eq!(verified.claims().source_tenant_id, c.source_tenant_id);
    assert_eq!(verified.claims().target_tenant_id, c.target_tenant_id);
    assert_eq!(verified.claims().command_id, c.command_id);
}
#[test]
fn wrong_issuer_company_and_audience_are_rejected_even_with_a_valid_signature() {
    let good = claims();
    for alter in [
        |c: &mut DelegationClaims| c.iss = "kabipay-client".into(),
        |c: &mut DelegationClaims| c.target_tenant_id = TenantId::try_from(id(70)).unwrap(),
        |c: &mut DelegationClaims| c.aud = "kabipay-payroll".into(),
    ] {
        let mut bad = good.clone();
        alter(&mut bad);
        assert!(verifier().verify(&token(&bad), &expected(&good)).is_err());
    }
}
#[test]
fn body_resource_capability_command_and_version_are_all_bound_to_the_token() {
    let good = claims();
    for alter in [
        |c: &mut DelegationClaims| c.request_hash = command_body_hash(b"reject"),
        |c: &mut DelegationClaims| c.resource_id = id(90),
        |c: &mut DelegationClaims| c.resource_type = "payroll_run".into(),
        |c: &mut DelegationClaims| c.capability = Capability::parse("payroll:manage").unwrap(),
        |c: &mut DelegationClaims| c.command_id = id(100),
        |c: &mut DelegationClaims| c.expected_resource_version = version(4),
    ] {
        let mut bad = good.clone();
        alter(&mut bad);
        assert!(verifier().verify(&token(&bad), &expected(&good)).is_err());
    }
}
#[test]
fn expired_future_and_excessively_long_lived_tokens_fail() {
    let good = claims();
    for alter in [
        |c: &mut DelegationClaims| {
            c.exp = c.iat - 1;
            c.iat -= 120;
            c.nbf = c.iat;
        },
        |c: &mut DelegationClaims| {
            c.iat += 60;
            c.nbf = c.iat;
            c.exp = c.iat + 120;
        },
        |c: &mut DelegationClaims| c.exp = c.iat + MAX_DELEGATION_LIFETIME_SECS + 1,
        |c: &mut DelegationClaims| c.nbf = c.exp + 1,
    ] {
        let mut bad = good.clone();
        alter(&mut bad);
        assert!(verifier().verify(&token(&bad), &expected(&good)).is_err());
    }
}
#[test]
fn malformed_ids_hashes_and_resource_names_fail_and_cannot_be_issued() {
    let good = claims();
    let issuer = DelegationIssuer::new("a", PRIVATE_A).unwrap();
    for alter in [
        |c: &mut DelegationClaims| c.jti = Uuid::nil(),
        |c: &mut DelegationClaims| c.source_user_id = Uuid::nil(),
        |c: &mut DelegationClaims| c.authority_ref = Uuid::nil(),
        |c: &mut DelegationClaims| c.command_id = Uuid::nil(),
        |c: &mut DelegationClaims| c.resource_id = Uuid::nil(),
        |c: &mut DelegationClaims| c.request_hash = "not-a-digest".into(),
        |c: &mut DelegationClaims| c.resource_type = "leave request".into(),
    ] {
        let mut bad = good.clone();
        alter(&mut bad);
        assert!(issuer.issue(&bad).is_err());
        assert!(verifier().verify(&token(&bad), &expected(&good)).is_err());
    }
}
#[test]
fn missing_token_type_key_id_and_unknown_keys_are_rejected() {
    let good = claims();
    for (kid, typ) in [
        (None, Some(DELEGATION_TOKEN_TYPE)),
        (Some("a"), None),
        (Some("a"), Some("JWT")),
        (Some("unknown"), Some(DELEGATION_TOKEN_TYPE)),
    ] {
        let mut h = Header::new(Algorithm::RS256);
        h.kid = kid.map(str::to_owned);
        h.typ = typ.map(str::to_owned);
        let t = encode(&h, &good, &EncodingKey::from_rsa_pem(PRIVATE_A).unwrap()).unwrap();
        assert!(verifier().verify(&t, &expected(&good)).is_err());
    }
}
#[test]
fn hs256_and_public_key_as_hmac_secret_cannot_confuse_the_verifier() {
    let good = claims();
    let mut h = Header::new(Algorithm::HS256);
    h.kid = Some("a".into());
    h.typ = Some(DELEGATION_TOKEN_TYPE.into());
    let t = encode(&h, &good, &EncodingKey::from_secret(PUBLIC_A)).unwrap();
    assert!(verifier().verify(&t, &expected(&good)).is_err());
}
#[test]
fn key_rotation_accepts_only_the_configured_key_and_rejects_mislabelled_signatures() {
    let good = claims();
    let rotated = DelegationVerifier::new(
        good.target_tenant_id,
        "kabipay-leave",
        vec![("a", PUBLIC_A), ("b", PUBLIC_B)],
    )
    .unwrap();
    let t = DelegationIssuer::new("b", PRIVATE_B)
        .unwrap()
        .issue(&good)
        .unwrap();
    assert!(rotated.verify(&t, &expected(&good)).is_ok());
    assert!(verifier().verify(&t, &expected(&good)).is_err());
    let t = DelegationIssuer::new("a", PRIVATE_B)
        .unwrap()
        .issue(&good)
        .unwrap();
    assert!(rotated.verify(&t, &expected(&good)).is_err());
}
#[test]
fn empty_duplicate_and_wildcard_key_configuration_is_rejected() {
    let target = claims().target_tenant_id;
    assert!(DelegationVerifier::new(target, "kabipay-leave", vec![]).is_err());
    assert!(DelegationVerifier::new(target, "*", vec![("a", PUBLIC_A)]).is_err());
    assert!(DelegationVerifier::new(
        target,
        "kabipay-leave",
        vec![("a", PUBLIC_A), ("a", PUBLIC_B)]
    )
    .is_err());
    assert!(DelegationIssuer::new("", PRIVATE_A).is_err());
    assert!(DelegationVerifier::new(target, "kabipay-leave", vec![("a", PRIVATE_A)]).is_err());
}
