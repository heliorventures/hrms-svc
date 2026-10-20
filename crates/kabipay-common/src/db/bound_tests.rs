use super::*;
use crate::db::{resolve_required_tenant_handle, resolve_tenant_handle, TenantDbConfig};
use sea_orm::DatabaseConnection;

fn id() -> Uuid {
    Uuid::from_u128(1)
}
fn binding() -> BoundTenantDatabase {
    BoundTenantDatabase::from_values(
        Some(&id().to_string()),
        Some("postgres://fixture:fixture-pass@127.0.0.1:1/bound_fixture"),
        Some("tenant_fixture"),
    )
    .unwrap()
    .unwrap()
}
fn fallback() -> TenantDbConfig {
    TenantDbConfig {
        db_host: "registry-host-must-not-be-used".into(),
        db_port: 1,
        db_name: "other".into(),
        db_user: "fixture".into(),
        db_password: "unused".into(),
        schema_name: "unused".into(),
    }
}
#[test]
fn partial_or_invalid_binding_configuration_fails_instead_of_enabling_legacy_routing() {
    assert!(BoundTenantDatabase::from_values(None, None, None)
        .unwrap()
        .is_none());
    for values in [
        (Some(""), None, None),
        (Some("invalid"), Some(""), Some("tenant")),
        (
            Some("00000000-0000-0000-0000-000000000000"),
            Some("postgres://u:p@localhost/fixture"),
            Some("tenant"),
        ),
        (
            Some("00000000-0000-0000-0000-000000000001"),
            Some("postgres://u:p@localhost/fixture"),
            Some("public"),
        ),
    ] {
        assert!(BoundTenantDatabase::from_values(values.0, values.1, values.2).is_err());
    }
}
#[test]
fn bound_database_debug_does_not_expose_credentials_or_connection_urls() {
    let value = format!("{:?}", binding());
    assert!(!value.contains("fixture-pass"));
    assert!(!value.contains("postgres://"));
}
#[test]
fn bound_urls_require_explicit_connection_coordinates_and_known_query_options() {
    for url in [
        "postgres://u:p@localhost/",
        "postgres:///fixture",
        "postgres://localhost/fixture",
        "postgres://u:p@localhost/fixture?host=other",
        "postgres://u:p@localhost/fixture?unknown=must-not-be-logged",
    ] {
        assert!(BoundTenantDatabase::from_values(
            Some(&id().to_string()),
            Some(url),
            Some("tenant_fixture")
        )
        .is_err());
    }
    for schema in [
        "",
        "kabipay_ops",
        "information_schema",
        "pg_catalog",
        "tenant;DROP",
        "1tenant",
    ] {
        assert!(BoundTenantDatabase::from_values(
            Some(&id().to_string()),
            Some("postgres://u:p@localhost/fixture"),
            Some(schema)
        )
        .is_err());
    }
}
#[test]
fn registered_mapping_must_match_all_fixed_connection_coordinates() {
    let value = binding();
    assert!(value.matches_mapping("127.0.0.1", "bound_fixture", "tenant_fixture"));
    assert!(!value.matches_mapping("other-host", "bound_fixture", "tenant_fixture"));
    assert!(!value.matches_mapping("127.0.0.1", "other_database", "tenant_fixture"));
    assert!(!value.matches_mapping("127.0.0.1", "bound_fixture", "other_schema"));
}
#[tokio::test]
async fn wrong_company_is_denied_before_cached_handles_or_control_plane_lookup() {
    let cache = TenantDbCache::with_binding(binding());
    let foreign = Uuid::from_u128(2);
    cache.inner.insert(
        foreign,
        TenantDbHandle {
            conn: DatabaseConnection::Disconnected,
            schema_name: "foreign".into(),
            db_host: "foreign".into(),
            db_name: "foreign".into(),
            from_ops_row: true,
        },
    );
    for result in [
        resolve_required_tenant_handle(
            foreign,
            &DatabaseConnection::Disconnected,
            &cache,
            &fallback(),
        )
        .await,
        resolve_tenant_handle(
            foreign,
            &DatabaseConnection::Disconnected,
            &cache,
            &fallback(),
        )
        .await,
    ] {
        assert!(matches!(result, Err(KabiPayError::Forbidden(_))));
    }
}
#[tokio::test]
async fn configured_company_reuses_its_connection_without_querying_the_registry() {
    let cache = TenantDbCache::with_binding(binding());
    cache.inner.insert(
        id(),
        TenantDbHandle {
            conn: DatabaseConnection::Disconnected,
            schema_name: "tenant_fixture".into(),
            db_host: "127.0.0.1".into(),
            db_name: "bound_fixture".into(),
            from_ops_row: false,
        },
    );
    let handle = resolve_required_tenant_handle(
        id(),
        &DatabaseConnection::Disconnected,
        &cache,
        &fallback(),
    )
    .await
    .unwrap();
    assert_eq!(handle.schema_name, "tenant_fixture");
    assert_eq!(handle.db_name, "bound_fixture");
    cache.invalidate(id());
    assert!(cache.is_empty());
}
