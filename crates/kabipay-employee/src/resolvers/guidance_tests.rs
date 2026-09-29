use async_graphql::{Context, EmptyMutation, EmptySubscription, Object, Request, Result, Schema};
use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
use kabipay_common::db::{TenantDbCache, TenantDbConfig};
use kabipay_common::subgraph::TenantId;
use crate::services::guidance_service::test_support::connection as guidance_connection;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
    ProxyRow, Statement,
};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use uuid::Uuid;

use super::{mutation::MutationRoot, query::QueryRoot};

#[derive(Clone)]
struct GuidanceQueryWithProxy {
    db: DatabaseConnection,
}

#[Object]
impl GuidanceQueryWithProxy {
    async fn my_guidance_state(
        &self,
        ctx: &Context<'_>,
    ) -> Result<crate::resolvers::types::MyGuidanceStateDto> {
        let (tenant_id, user_id) = super::query::authenticated_guidance_identity(ctx)?;
        super::query::load_guidance_state(&self.db, tenant_id, user_id).await
    }
}

fn client_claims(tenant_id: Uuid, user_id: Uuid) -> ClientClaims {
    ClientClaims {
        sub: user_id,
        iss: CLIENT_JWT_ISSUER.into(),
        exp: 0,
        iat: 0,
        tenant_id,
        email: String::new(),
        employee_id: None,
        must_change_password: false,
        roles: Vec::new(),
        permissions: Vec::new(),
        permission_scopes: HashMap::new(),
        resource_scopes: HashMap::new(),
    }
}

fn guidance_success_schema(
    tenant_id: Uuid,
    user_id: Uuid,
    db: DatabaseConnection,
) -> Schema<GuidanceQueryWithProxy, EmptyMutation, EmptySubscription> {
    Schema::build(GuidanceQueryWithProxy { db }, EmptyMutation, EmptySubscription)
        .data(TenantId(tenant_id))
        .data(client_claims(tenant_id, user_id))
        .finish()
}

#[derive(Debug)]
struct OpsLookupProxy {
    statements: Arc<Mutex<Vec<Statement>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for OpsLookupProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements
            .lock()
            .expect("ops statement recorder")
            .push(statement);
        Ok(Vec::new())
    }

    async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom(
            "guidance resolver authorization test unexpectedly wrote to ops DB".into(),
        ))
    }
}

async fn authenticated_schema(
    tenant_id: Uuid,
    user_id: Uuid,
) -> (Schema<QueryRoot, MutationRoot, EmptySubscription>, Arc<Mutex<Vec<Statement>>>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let ops_db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(OpsLookupProxy {
            statements: Arc::clone(&statements),
        })),
    )
    .await
    .expect("ops proxy connection");
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription)
        .data(TenantId(tenant_id))
        .data(client_claims(tenant_id, user_id))
        .data(TenantDbCache::new())
        .data(ops_db)
        .data(TenantDbConfig {
            db_host: "unused.invalid".into(),
            db_port: 5432,
            db_name: "unused".into(),
            db_user: "unused".into(),
            db_password: "unused".into(),
            schema_name: "unused".into(),
        })
        .finish();
    (schema, statements)
}

fn assert_authenticated_tenant_lookup(
    response: &async_graphql::Response,
    statements: &Arc<Mutex<Vec<Statement>>>,
    tenant_id: Uuid,
) {
    assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
    assert_eq!(
        response.errors[0].extensions.as_ref().unwrap().get("code"),
        Some(&async_graphql::Value::from("TENANT_DATABASE_UNAVAILABLE"))
    );

    let statements = statements.lock().expect("ops statement recorder");
    assert_eq!(statements.len(), 1, "expected only the authenticated tenant lookup");
    let lookup = &statements[0];
    assert!(lookup.sql.contains("tenant_database"), "sql={}", lookup.sql);
    assert!(
        lookup
            .values
            .as_ref()
            .map(|values| values.0.contains(&sea_orm::Value::from(tenant_id)))
            .unwrap_or(false),
        "tenant lookup did not bind the authenticated tenant ID: {lookup:?}"
    );
}

#[tokio::test]
async fn reading_guidance_state_requires_authenticated_client_claims() {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();

    let response = schema
        .execute(Request::new(
            "{ myGuidanceState { overviewDismissedAt } }",
        ))
        .await;

    assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
    assert_eq!(
        response.errors[0].extensions.as_ref().unwrap().get("code"),
        Some(&async_graphql::Value::from("UNAUTHENTICATED"))
    );
}

#[tokio::test]
async fn dismissing_guidance_requires_authenticated_client_claims() {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();

    let response = schema
        .execute(Request::new(
            "mutation { dismissMyApplicationOverview { overviewDismissedAt } }",
        ))
        .await;

    assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
    assert_eq!(
        response.errors[0].extensions.as_ref().unwrap().get("code"),
        Some(&async_graphql::Value::from("UNAUTHENTICATED"))
    );
}

#[tokio::test]
async fn authenticated_guidance_read_uses_tenant_context_for_tenant_lookup() {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let (schema, statements) = authenticated_schema(tenant_id, user_id).await;

    let response = schema
        .execute(Request::new(
            "{ myGuidanceState { overviewDismissedAt } }",
        ))
        .await;

    assert_authenticated_tenant_lookup(&response, &statements, tenant_id);
}

#[tokio::test]
async fn authenticated_guidance_dismissal_uses_tenant_context_for_tenant_lookup() {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let (schema, statements) = authenticated_schema(tenant_id, user_id).await;

    let response = schema
        .execute(Request::new(
            "mutation { dismissMyApplicationOverview { overviewDismissedAt } }",
        ))
        .await;

    assert_authenticated_tenant_lookup(&response, &statements, tenant_id);
}

#[tokio::test]
async fn authenticated_guidance_read_returns_null_for_the_claimed_user_without_a_row() {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let (db, store) = guidance_connection(&[tenant_id], &[user_id], 1_790_000_400).await;
    let schema = guidance_success_schema(tenant_id, user_id, db);

    let response = schema
        .execute(Request::new("{ myGuidanceState { overviewDismissedAt } }"))
        .await;

    assert!(response.errors.is_empty(), "unexpected response: {response:?}");
    let data = response.data.into_json().expect("JSON GraphQL response");
    assert_eq!(data["myGuidanceState"]["overviewDismissedAt"], serde_json::Value::Null);
    assert_eq!(store.row_count(), 0);
    let statement = store
        .statements()
        .into_iter()
        .find(|statement| statement.sql.to_ascii_uppercase().contains("SELECT"))
        .expect("guidance state SELECT");
    let bound_values = statement.values.expect("bound guidance identity").0;
    assert!(bound_values.contains(&sea_orm::Value::from(tenant_id)));
    assert!(bound_values.contains(&sea_orm::Value::from(user_id)));
}

#[tokio::test]
async fn authenticated_guidance_read_returns_the_claimed_users_dismissal_timestamp() {
    let tenant_id = Uuid::new_v4();
    let user_id = Uuid::new_v4();
    let (db, store) = guidance_connection(&[tenant_id], &[user_id], 1_790_000_500).await;
    let dismissed_at = crate::services::guidance_service::dismiss_overview(&db, tenant_id, user_id)
        .await
        .expect("seed the claimed user's dismissal through the service");
    let schema = guidance_success_schema(tenant_id, user_id, db);

    let response = schema
        .execute(Request::new("{ myGuidanceState { overviewDismissedAt } }"))
        .await;

    assert!(response.errors.is_empty(), "unexpected response: {response:?}");
    let data = response.data.into_json().expect("JSON GraphQL response");
    assert_eq!(
        data["myGuidanceState"]["overviewDismissedAt"],
        serde_json::Value::String(dismissed_at.to_rfc3339())
    );
    assert_eq!(store.row_count(), 1);
    assert_eq!(store.timestamp(tenant_id, user_id), Some(dismissed_at));
    let read = store
        .statements()
        .into_iter()
        .filter(|statement| statement.sql.to_ascii_uppercase().contains("SELECT"))
        .last()
        .expect("authenticated guidance state SELECT");
    let bound_values = read.values.expect("bound authenticated identity").0;
    assert!(bound_values.contains(&sea_orm::Value::from(tenant_id)));
    assert!(bound_values.contains(&sea_orm::Value::from(user_id)));
}
