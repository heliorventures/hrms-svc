use super::QueryRoot;
use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema, Value};
use kabipay_common::context::{ClientClaims, OperatorContext};
use sea_orm::entity::prelude::async_trait;
use sea_orm::{Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
use std::sync::{Arc, Mutex};
use uuid::Uuid;

#[derive(Debug, Clone, Default)]
struct ReadRecorder(Arc<Mutex<Vec<String>>>);

#[async_trait::async_trait]
impl ProxyDatabaseTrait for ReadRecorder {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.0.lock().expect("query recorder").push(statement.to_string());
        Ok(vec![])
    }

    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("operator list requests must never write")
    }
}

async fn schema() -> (Schema<QueryRoot, EmptyMutation, EmptySubscription>, ReadRecorder) {
    let recorder = ReadRecorder::default();
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(recorder.clone())))
        .await.expect("fixture connection");
    (Schema::build(QueryRoot, EmptyMutation, EmptySubscription).data(db).finish(), recorder)
}

fn operator(tenants: Vec<Uuid>) -> OperatorContext {
    OperatorContext { operator_user_id: Uuid::new_v4(), roles: vec![], tenant_access: tenants }
}

fn client() -> ClientClaims {
    ClientClaims { sub: Uuid::new_v4(), iss: "kabipay-client".into(), exp: i64::MAX,
        iat: 0, tenant_id: Uuid::new_v4(), email: String::new(), employee_id: None,
        must_change_password: false, roles: vec!["ADMIN".into()], permissions: vec![],
        permission_scopes: Default::default(), resource_scopes: Default::default() }
}

fn assert_forbidden(response: &async_graphql::Response, query: &str) {
    assert_eq!(response.errors.len(), 1, "{query}: {:?}", response.errors);
    assert_eq!(response.errors[0].extensions.as_ref().and_then(|e| e.get("code")),
        Some(&Value::from("FORBIDDEN")), "{query}: {:?}", response.errors);
}

#[tokio::test]
async fn business_lists_deny_anonymous_and_company_admin_before_sql() {
    let (schema, recorder) = schema().await;
    for field in ["tenants", "tenantSubscriptions", "operatorUsers", "operatorRoles", "invoices", "payments", "billingCycles"] {
        let query = format!("{{ {field}(limit: 1) {{ __typename }} }}");
        assert_forbidden(&schema.execute(query.clone()).await, &query);
        assert_forbidden(&schema.execute(Request::new(query.clone()).data(client())).await, &query);
    }
    assert!(recorder.0.lock().expect("queries").is_empty());
}

#[tokio::test]
async fn scoped_operator_cannot_read_global_operator_directory() {
    let (schema, recorder) = schema().await;
    let tenant = Uuid::new_v4();
    for field in ["operatorUsers(limit:1)".to_owned(), "operatorRoles(limit:1)".to_owned(),
        format!("operatorRolesForUser(operatorUserId:\"{}\")", Uuid::new_v4())] {
        let query = format!("{{ {field} {{ __typename }} }}");
        assert_forbidden(&schema.execute(Request::new(query.clone()).data(operator(vec![tenant]))).await, &query);
    }
    assert!(recorder.0.lock().expect("queries").is_empty());
}

#[tokio::test]
async fn scoped_operator_explicit_foreign_tenant_is_denied_before_sql() {
    let (schema, recorder) = schema().await;
    for field in ["tenantSubscriptions", "invoices", "billingCycles", "featureFlags"] {
        let query = format!("{{ {field}(tenantId:\"{}\",limit:1) {{ __typename }} }}", Uuid::new_v4());
        assert_forbidden(&schema.execute(Request::new(query.clone()).data(operator(vec![Uuid::new_v4()]))).await, &query);
    }
    assert!(recorder.0.lock().expect("queries").is_empty());
}

#[tokio::test]
async fn scoped_lists_filter_authorized_tenants_before_limit_and_payment_invoice() {
    let (schema, recorder) = schema().await;
    let allowed = Uuid::new_v4();
    for field in ["tenants", "tenantSubscriptions", "invoices", "payments", "billingCycles"] {
        let query = format!("{{ {field}(limit:1) {{ __typename }} }}");
        let response = schema.execute(Request::new(query).data(operator(vec![allowed]))).await;
        assert!(response.errors.is_empty(), "{field}: {:?}", response.errors);
        let statement = recorder.0.lock().expect("queries").last().expect("query").clone();
        assert!(statement.contains(&allowed.to_string()), "{field}: {statement}");
        assert!(statement.find("WHERE").expect("tenant predicate") < statement.find("LIMIT").expect("bounded query"));
        if field == "payments" {
            assert!(statement.contains("invoice") && statement.contains("tenant_id"), "{statement}");
        }
    }
    let foreign_invoice = Uuid::new_v4();
    let response = schema.execute(Request::new(format!("{{ payments(invoiceId:\"{foreign_invoice}\",limit:1) {{ __typename }} }}"))
        .data(operator(vec![allowed]))).await;
    assert!(response.errors.is_empty());
    let statement = recorder.0.lock().expect("queries").last().expect("query").clone();
    assert!(statement.contains(&allowed.to_string()) && statement.contains(&foreign_invoice.to_string()));
}

#[tokio::test]
async fn global_operator_lists_and_public_health_catalog_still_work() {
    let (schema, recorder) = schema().await;
    let response = schema.execute("{ operatorHealth tenantHealth billingHealth modules(limit:1) { __typename } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    for field in ["tenants", "tenantSubscriptions", "operatorUsers", "operatorRoles", "invoices", "payments", "billingCycles"] {
        let response = schema.execute(Request::new(format!("{{ {field}(limit:1) {{ __typename }} }}"))
            .data(operator(vec![]))).await;
        assert!(response.errors.is_empty(), "{field}: {:?}", response.errors);
    }
    assert_eq!(recorder.0.lock().expect("queries").len(), 8);
}

#[tokio::test]
async fn anonymous_http_business_requests_are_denied_even_when_selecting_only_typename() {
    use async_graphql_axum::GraphQLRequest;
    use axum::{body::{Body, to_bytes}, extract::{FromRequest, State}, http::{HeaderMap, Request as HttpRequest}, response::IntoResponse};
    use kabipay_common::subgraph::tenant_graphql_post;
    let (schema, recorder) = schema().await;
    let schema = Arc::new(schema);
    for field in ["tenants", "tenantSubscriptions", "operatorUsers", "operatorRoles", "invoices", "payments", "billingCycles"] {
        let request = HttpRequest::builder().method("POST").header("content-type", "application/json")
            .body(Body::from(serde_json::json!({"query":format!("{{ {field}(limit:1) {{ __typename }} }}")}).to_string()))
            .expect("HTTP fixture");
        let request = GraphQLRequest::from_request(request, &()).await.ok().expect("JSON fixture");
        let response = tenant_graphql_post(State(schema.clone()), HeaderMap::new(), request).await.into_response();
        let bytes = to_bytes(response.into_body(), 16 * 1024).await.expect("bounded response");
        let body: serde_json::Value = serde_json::from_slice(&bytes).expect("GraphQL JSON");
        assert_eq!(body["errors"][0]["extensions"]["code"], "FORBIDDEN", "{field}: {body}");
        assert!(body["data"].is_null(), "{field}: {body}");
    }
    assert!(recorder.0.lock().expect("queries").is_empty());
}
