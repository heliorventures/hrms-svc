//! Directory mutation authority must use the action's scope, not role or read scope.
use async_graphql::{EmptySubscription, Request, Schema, Value};
use kabipay_common::context::ClientClaims;
use kabipay_employee::resolvers::{MutationRoot, QueryRoot};
use uuid::Uuid;

fn claims(scope: Option<&str>) -> ClientClaims {
    let mut scopes = std::collections::HashMap::new();
    if let Some(scope) = scope { scopes.insert("employee:write".into(), scope.into()); }
    scopes.insert("employee:read".into(), "ALL".into());
    ClientClaims { sub: Uuid::new_v4(), iss: "kabipay-client".into(), exp: i64::MAX,
        iat: 0, tenant_id: Uuid::new_v4(), email: String::new(), employee_id: None,
        must_change_password: false, roles: vec!["ADMIN".into()],
        permissions: vec!["employee:write".into(), "employee:read".into()],
        permission_scopes: scopes, resource_scopes: Default::default() }
}

#[tokio::test]
async fn directory_writes_deny_narrow_or_missing_action_scope_before_database() {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();
    for scope in [None, Some("SELF"), Some("TEAM"), Some("DEPARTMENT"), Some("INVALID")] {
        for query in [
            "mutation { createEmployee(input:{employeeCode:\"SCOPE-TEST\",firstName:\"Test\",lastName:\"User\",dateOfJoining:\"2026-11-13\"}) { id } }".to_owned(),
            format!("mutation {{ updateEmployee(input:{{id:\"{}\",firstName:\"Changed\"}}) {{ id }} }}", Uuid::new_v4()),
        ] {
            let response = schema.execute(Request::new(query.clone()).data(claims(scope))).await;
            assert_eq!(response.errors.len(), 1, "{scope:?}: {query}: {:?}", response.errors);
            assert_eq!(response.errors[0].extensions.as_ref().and_then(|e| e.get("code")),
                Some(&Value::from("FORBIDDEN")), "{scope:?}: {query}: {:?}", response.errors);
        }
    }
}
