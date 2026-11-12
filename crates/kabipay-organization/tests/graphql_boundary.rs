use kabipay_organization::{graphql::build_schema, store::Store};
use sea_orm::DatabaseConnection;
#[tokio::test]
async fn anonymous_schema_discovery_does_not_authorize_business_fields() {
    let schema = build_schema(Store::new(DatabaseConnection::Disconnected));
    assert!(schema
        .execute("{ __schema { queryType { name } } }")
        .await
        .errors
        .is_empty());
    let denied = schema.execute("{ myOrganizations { id } }").await;
    assert_eq!(
        denied.errors.len(),
        1,
        "anonymous requests must fail before database access"
    );
    assert_eq!(
        denied.errors[0].message,
        "unauthorised — invalid or missing token"
    );
}

#[tokio::test]
async fn gateway_schema_discovery_fits_the_organization_request_limits() {
    let schema = build_schema(Store::new(DatabaseConnection::Disconnected));
    let response = schema
        .execute(include_str!("fixtures/gateway-introspection.graphql"))
        .await;
    assert!(
        response.errors.is_empty(),
        "gateway discovery must respect the existing depth and complexity limits: {:?}",
        response.errors
    );
    assert!(response.data.into_json().unwrap()["__schema"].is_object());
}
