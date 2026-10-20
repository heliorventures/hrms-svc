use super::*;
use crate::db::BoundTenantDatabase;

fn cache() -> TenantDbCache {
    TenantDbCache::with_binding(
        BoundTenantDatabase::from_values(
            Some("00000000-0000-0000-0000-000000000001"),
            Some("postgres://fixture:synthetic@127.0.0.1:1/fixture"),
            Some("tenant_fixture"),
        )
        .unwrap()
        .unwrap(),
    )
}
fn client(tenant: u128, authenticated: bool) -> RequestIdentity {
    let claims=authenticated.then(||serde_json::from_value(serde_json::json!({"sub":Uuid::from_u128(5),"iss":"kabipay-client","exp":i64::MAX,"iat":1,"tenant_id":Uuid::from_u128(tenant)})).unwrap());
    RequestIdentity::Client {
        tenant_id: Uuid::from_u128(tenant),
        claims,
    }
}
#[test]
fn bound_graphql_accepts_only_authenticated_clients_from_the_configured_company() {
    let cache = cache();
    assert!(validate_bound_identity(Some(&cache), &client(1, true)).is_ok());
    assert!(matches!(
        validate_bound_identity(Some(&cache), &client(2, true)),
        Err(KabiPayError::Forbidden(_))
    ));
    assert!(matches!(
        validate_bound_identity(Some(&cache), &client(1, false)),
        Err(KabiPayError::Unauthorised)
    ));
    assert!(matches!(
        validate_bound_identity(Some(&cache), &RequestIdentity::None),
        Err(KabiPayError::Unauthorised)
    ));
}
#[test]
fn operator_tokens_do_not_enter_a_bound_company_graphql_runtime() {
    let operator=serde_json::from_value(serde_json::json!({"sub":Uuid::from_u128(6),"iss":"kabipay-ops","exp":i64::MAX,"iat":1,"roles":[],"tenant_access":[]})).unwrap();
    assert!(matches!(
        validate_bound_identity(Some(&cache()), &RequestIdentity::Operator(operator)),
        Err(KabiPayError::Forbidden(_))
    ));
}
#[test]
fn legacy_unbound_runtime_keeps_its_existing_identity_handling() {
    for identity in [client(1, false), client(2, true), RequestIdentity::None] {
        assert!(validate_bound_identity(Some(&TenantDbCache::new()), &identity).is_ok());
        assert!(validate_bound_identity(None, &identity).is_ok());
    }
}
