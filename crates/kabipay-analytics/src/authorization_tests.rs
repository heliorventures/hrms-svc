use crate::resolvers::{MutationRoot, QueryRoot};
use async_graphql::{EmptySubscription, Request, Schema, Value};
use kabipay_common::context::ClientClaims;
use uuid::Uuid;

fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
    let mut scopes = std::collections::HashMap::new();
    if let Some(scope) = scope { scopes.insert(permission.to_owned(), scope.to_owned()); }
    ClientClaims { sub: Uuid::new_v4(), iss: "kabipay-client".into(), exp: i64::MAX, iat: 0,
        tenant_id: Uuid::new_v4(), email: String::new(), employee_id: None,
        must_change_password: false, roles: vec!["ADMIN".into()], permissions: vec![permission.into()],
        permission_scopes: scopes, resource_scopes: Default::default() }
}

#[tokio::test]
async fn tenant_integration_and_history_actions_require_directory_all_scope_before_sql() {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();
    let id = Uuid::new_v4();
    let mut operations: Vec<String> = ["outboxEvents", "integrationConnectors", "tenantIntegrations", "webhookSubscriptions", "webhookDeliveryLogs", "auditLogs"]
        .into_iter().map(|field| format!("{{ {field}(limit:1) {{ __typename }} }}")).collect();
    operations.extend([
        format!("mutation {{ requeueOutboxEvent(id:\"{id}\") {{ __typename }} }}"),
        format!("mutation {{ connectTenantIntegration(connectorId:\"{id}\") {{ __typename }} }}"),
        "mutation { registerWebhookSubscription(input:{eventName:\"employee.updated\",endpointUrl:\"https://example.invalid/fixture\"}) { __typename } }".into(),
        format!("mutation {{ setWebhookSubscriptionActive(id:\"{id}\",active:true) {{ __typename }} }}"),
    ]);
    for permission in ["employee:write", "employee:manage"] {
        for scope in [None, Some("SELF"), Some("TEAM"), Some("DEPARTMENT"), Some("INVALID")] {
            for query in &operations {
                let response = schema.execute(Request::new(query.clone()).data(claims(permission, scope))).await;
                assert_eq!(response.errors.len(), 1, "{permission}/{scope:?}: {query}: {:?}", response.errors);
                assert_eq!(response.errors[0].extensions.as_ref().and_then(|e| e.get("code")),
                    Some(&Value::from("FORBIDDEN")), "{permission}/{scope:?}: {query}: {:?}", response.errors);
            }
        }
    }
}
