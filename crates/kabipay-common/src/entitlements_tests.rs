use super::*;

#[tokio::test]
async fn control_plane_load_is_tenant_scoped_and_fail_closed() {
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{Database, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow};
    use std::{collections::{BTreeMap, VecDeque}, sync::{Arc, Mutex}};
    #[derive(Clone, Debug)]
    struct Proxy {
        replies: Arc<Mutex<VecDeque<Vec<ProxyRow>>>>,
        sql: Arc<Mutex<Vec<String>>>,
    }
    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for Proxy {
        async fn query(&self, sql: Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.sql.lock().unwrap().push(sql.to_string());
            self.replies.lock().unwrap().pop_front().ok_or_else(|| DbErr::Custom("control plane unavailable".into()))
        }
        async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
            panic!("authorization must not write")
        }
    }
    let tenant_id = Uuid::new_v4();
    let now = chrono::Utc::now();
    let mut fields = BTreeMap::from([
        ("id".into(), tenant_id.into()), ("name".into(), "fixture".into()),
        ("status".into(), "ACTIVE".into()), ("is_deleted".into(), false.into()),
        ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
        ("account_manager_id".into(), Option::<Uuid>::None.into()),
        ("deleted_by".into(), Option::<Uuid>::None.into()),
        ("deleted_at".into(), Option::<chrono::DateTime<chrono::Utc>>::None.into()),
    ]);
    for name in ["plan", "country", "timezone", "currency", "gstin", "pan", "registered_address", "logo_url", "primary_color", "subdomain"] {
        fields.insert(name.into(), Option::<String>::None.into());
    }
    let active = ProxyRow::new(fields.clone());
    fields.insert("status".into(), "SUSPENDED".into());
    let proxy = Proxy {
        replies: Arc::new(Mutex::new(VecDeque::from([
            vec![active], vec![], vec![], vec![], vec![],
            vec![ProxyRow::new(fields)], vec![],
        ]))), sql: Arc::new(Mutex::new(Vec::new())),
    };
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy.clone()))).await.unwrap();
    let state = Entitlements::load(&db, tenant_id).await.unwrap();
    assert!(!state.allows("LEAVE"));
    let sql = proxy.sql.lock().unwrap().clone();
    assert_eq!(sql.len(), 5);
    for query in &sql[..3] { assert!(query.contains(&tenant_id.to_string()), "{query}"); }
    assert!(sql[1].contains("is_deleted"));
    assert!(matches!(Entitlements::load(&db, tenant_id).await, Err(KabiPayError::TenantSuspended(_))));
    assert!(matches!(Entitlements::load(&db, Uuid::new_v4()).await, Err(KabiPayError::TenantSuspended(_))));
    assert!(Entitlements::load(&db, tenant_id).await.is_err());
}

fn snapshot(core: bool) -> Entitlements {
    Entitlements {
        tenant_id: Uuid::new_v4(), today: NaiveDate::from_ymd_opt(2026, 10, 20).unwrap(),
        modules: HashMap::from([("LEAVE".into(), ModuleAccess {
            id: Uuid::new_v4(), active: true, core, disabled: false,
            subscription: None,
        })]), dependencies: HashMap::new(),
    }
}

#[test]
fn core_needs_no_subscription_but_still_respects_catalog_and_deny_flag() {
    let mut state = snapshot(true);
    assert!(state.allows("LEAVE"));
    state.modules.get_mut("LEAVE").unwrap().disabled = true;
    assert!(!state.allows("LEAVE"));
    state.modules.get_mut("LEAVE").unwrap().disabled = false;
    state.modules.get_mut("LEAVE").unwrap().active = false;
    assert!(!state.allows("LEAVE"));
}

#[test]
fn optional_requires_active_subscription_and_exclusive_expiry() {
    let mut state = snapshot(false);
    assert!(!state.allows("LEAVE"));
    let today = state.today;
    for (active, starts, expires, expected) in [
        (true, None, None, true),
        (false, None, None, false),
        (true, Some(today), None, true),
        (true, Some(today.succ_opt().unwrap()), None, false),
        (true, None, Some(today), false),
        (true, None, Some(today.succ_opt().unwrap()), true),
        (true, Some(today), Some(today), false),
    ] {
        state.modules.get_mut("LEAVE").unwrap().subscription = Some(SubscriptionAccess { active, starts, expires });
        assert_eq!(state.allows("LEAVE"), expected);
    }
}

#[test]
fn dependency_cycles_missing_dependencies_and_revocation_deny_access() {
    let mut state = snapshot(true);
    let id = state.modules["LEAVE"].id;
    state.dependencies.insert(id, vec![id]);
    assert!(!state.allows("LEAVE"));
    let other = Uuid::new_v4();
    state.dependencies.insert(id, vec![other]);
    assert!(!state.allows("LEAVE"));
    state.modules.insert("EMPLOYEE".into(), ModuleAccess {
        id: other, active: true, core: true, disabled: false, subscription: None,
    });
    assert!(state.allows("LEAVE"));
    state.modules.get_mut("EMPLOYEE").unwrap().disabled = true;
    assert!(!state.allows("LEAVE"));
}

#[test]
fn tenant_substitution_unknown_module_and_unknown_service_fail_closed() {
    let state = snapshot(true);
    assert!(state.require_tenant(state.tenant_id).is_ok());
    assert!(state.require_tenant(Uuid::new_v4()).is_err());
    assert!(!state.allows("UNKNOWN"));
    assert!(service_module("kabipay-new-service").is_err());
    assert_eq!(service_module("kabipay-notification").unwrap(), Some("EMPLOYEE"));
    assert_eq!(service_module("kabipay-ops").unwrap(), None);
}

#[test]
fn report_permissions_are_removed_for_unavailable_domains() {
    let state = snapshot(true);
    let claims = ClientClaims {
        sub: Uuid::new_v4(), tenant_id: state.tenant_id, iss: "kabipay-client".into(),
        exp: 0, iat: 0, email: String::new(), employee_id: None, must_change_password: false,
        roles: vec!["ADMIN".into()], permissions: vec!["leave:read".into(), "payroll:read".into()],
        permission_scopes: HashMap::from([("leave:read".into(), "ALL".into()), ("payroll:read".into(), "ALL".into())]),
        resource_scopes: HashMap::from([("leave".into(), "ALL".into()), ("payroll".into(), "ALL".into())]),
    };
    let filtered = state.filter_claims(&claims).unwrap();
    assert_eq!(filtered.permissions, vec!["leave:read"]);
    assert!(!filtered.permission_scopes.contains_key("payroll:read"));
    assert!(!filtered.resource_scopes.contains_key("payroll"));
    assert!(filtered.resource_scopes.contains_key("leave"));
    assert_eq!(claims.permissions.len(), 2);
    assert_eq!(state.outbox_aggregates(), vec!["leave_request"]);
}

#[tokio::test]
async fn graphql_entitlements_block_business_resolution_but_preserve_schema_discovery() {
    use async_graphql::{Context, EmptySubscription, Object, Request, Schema};
    use std::sync::{Arc, atomic::{AtomicUsize, Ordering}};
    struct Query;
    #[Object]
    impl Query {
        async fn business(&self, ctx: &Context<'_>) -> bool {
            ctx.data_unchecked::<Arc<AtomicUsize>>().fetch_add(1, Ordering::SeqCst);
            true
        }
    }
    struct Mutation;
    #[Object]
    impl Mutation {
        async fn change(&self, ctx: &Context<'_>) -> bool {
            ctx.data_unchecked::<Arc<AtomicUsize>>().fetch_add(1, Ordering::SeqCst);
            true
        }
    }
    let hits = Arc::new(AtomicUsize::new(0));
    let schema = Schema::build(Query, Mutation, EmptySubscription)
        .enable_federation()
        .extension(crate::entitlement_graphql::ModuleEntitlement("LEAVE"))
        .data(Arc::clone(&hits)).finish();
    let mut state = snapshot(false);
    let claims = ClientClaims {
        sub: Uuid::new_v4(), tenant_id: state.tenant_id, iss: "kabipay-client".into(),
        exp: 0, iat: 0, email: String::new(), employee_id: None, must_change_password: false,
        roles: Vec::new(), permissions: Vec::new(), permission_scopes: HashMap::new(), resource_scopes: HashMap::new(),
    };
    let response = schema.execute("{ __schema { queryType { name } } _service { sdl } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert!(!schema.execute("{ alias: business }").await.errors.is_empty());
    assert!(!schema.execute("{ __schema: business }").await.errors.is_empty());
    assert!(!schema.execute("{ __schema { queryType { name } } business }").await.errors.is_empty());
    assert!(!schema.execute("mutation { change }").await.errors.is_empty());
    let request = |state: Entitlements, tenant_id| Request::new("{ ...Fields } fragment Fields on Query { alias: business }")
        .data(claims.clone()).data(crate::subgraph::TenantId(tenant_id)).data(state);
    let denied = schema.execute(request(state.clone(), state.tenant_id)).await;
    assert_eq!(denied.errors[0].extensions.as_ref().unwrap().get("code"), Some(&async_graphql::Value::from("MODULE_NOT_SUBSCRIBED")));
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    state.modules.get_mut("LEAVE").unwrap().core = true;
    assert!(!schema.execute(request(state.clone(), Uuid::new_v4())).await.errors.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 0);
    let allowed = schema.execute(request(state.clone(), state.tenant_id)).await;
    assert!(allowed.errors.is_empty(), "{:?}", allowed.errors);
    assert_eq!(hits.load(Ordering::SeqCst), 1);
    state.modules.get_mut("LEAVE").unwrap().disabled = true;
    assert!(!schema.execute(request(state.clone(), state.tenant_id)).await.errors.is_empty());
    assert_eq!(hits.load(Ordering::SeqCst), 1);
}
