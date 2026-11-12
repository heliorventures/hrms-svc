//! Architectural regression: company data transactions must never load tenant metadata.
#[test]
fn loan_company_transaction_helpers_do_not_query_control_plane() {
    for (name, source) in [
        ("lifecycle", include_str!("../src/lifecycle.rs")),
        ("payroll_state", include_str!("../src/payroll_state.rs")),
        ("access", include_str!("../src/access.rs")),
        ("recovery", include_str!("../src/recovery.rs")),
    ] {
        assert!(
            !source.contains("kabipay_ops."),
            "{name} queries control-plane metadata through company storage"
        );
        assert!(
            !source.contains("Entitlements::load(tx"),
            "{name} loads entitlements through company storage"
        );
    }
}

use kabipay_common::{
    context::{ClientClaims, CLIENT_JWT_ISSUER},
    entitlements::Entitlements,
};
use kabipay_loans::{payroll_loan_state_with_actor, LoanActorScope, LoanPermission};
use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement, TransactionTrait,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug)]
struct MetadataProxy(Mutex<VecDeque<Vec<ProxyRow>>>);
#[async_trait::async_trait]
impl ProxyDatabaseTrait for MetadataProxy {
    async fn query(&self, _: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| DbErr::Custom("unexpected metadata read".into()))
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("metadata is read only")
    }
}

async fn metadata(tenant: Uuid, enabled: bool) -> Entitlements {
    let now = chrono::Utc::now();
    let mut fields = BTreeMap::from([
        ("id".into(), tenant.into()),
        ("name".into(), "isolated fixture".into()),
        ("status".into(), "ACTIVE".into()),
        ("is_deleted".into(), false.into()),
        ("created_at".into(), now.into()),
        ("updated_at".into(), now.into()),
        ("account_manager_id".into(), Option::<Uuid>::None.into()),
        ("deleted_by".into(), Option::<Uuid>::None.into()),
        (
            "deleted_at".into(),
            Option::<chrono::DateTime<chrono::Utc>>::None.into(),
        ),
    ]);
    for name in [
        "plan",
        "country",
        "timezone",
        "currency",
        "gstin",
        "pan",
        "registered_address",
        "logo_url",
        "primary_color",
        "subdomain",
    ] {
        fields.insert(name.into(), Option::<String>::None.into());
    }
    fields.insert("currency".into(), "INR".into());
    fields.insert("timezone".into(), "Asia/Kolkata".into());
    let modules = if enabled {
        vec![ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::new_v4().into()),
            ("code".into(), "LOANS".into()),
            ("name".into(), "Loans".into()),
            ("is_active".into(), true.into()),
            ("is_core".into(), true.into()),
            ("display_order".into(), 0i32.into()),
            ("category".into(), Option::<String>::None.into()),
            ("description".into(), Option::<String>::None.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]))]
    } else {
        vec![]
    };
    let proxy = MetadataProxy(Mutex::new(VecDeque::from([
        vec![ProxyRow::new(fields)],
        vec![],
        vec![],
        modules,
        vec![],
    ])));
    let ops = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy)))
        .await
        .unwrap();
    Entitlements::load(&ops, tenant).await.unwrap()
}

#[derive(Debug)]
struct CompanyProxy {
    queries: Arc<Mutex<Vec<String>>>,
    has_unresolved_loan: bool,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for CompanyProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        assert!(
            !sql.contains("kabipay_ops"),
            "company transaction crossed metadata boundary"
        );
        self.queries.lock().unwrap().push(sql.clone());
        if self.has_unresolved_loan && sql.contains("FROM loan_account") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "id".into(),
                Uuid::new_v4().into(),
            )]))]);
        }
        Ok(vec![])
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("state read must not write")
    }
}
fn actor(tenant: Uuid) -> LoanActorScope {
    let claims = ClientClaims {
        sub: Uuid::new_v4(),
        tenant_id: tenant,
        iss: CLIENT_JWT_ISSUER.into(),
        exp: 0,
        iat: 0,
        email: String::new(),
        employee_id: None,
        must_change_password: false,
        roles: vec![],
        permissions: vec!["loan:read".into()],
        permission_scopes: HashMap::from([("loan:read".into(), "ALL".into())]),
        resource_scopes: HashMap::new(),
    };
    LoanActorScope::from_verified_claims(&claims, tenant, LoanPermission::Read).unwrap()
}
async fn company(has_unresolved_loan: bool) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let queries = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(CompanyProxy {
            queries: queries.clone(),
            has_unresolved_loan,
        })),
    )
    .await
    .unwrap();
    (db, queries)
}
#[tokio::test]
async fn trusted_metadata_supplies_currency_without_company_metadata_queries() {
    let tenant = Uuid::new_v4();
    let context = metadata(tenant, true).await;
    let actor = actor(tenant).with_tenant_entitlements(context).unwrap();
    let (db, queries) = company(false).await;
    let tx = db.begin().await.unwrap();
    let state = payroll_loan_state_with_actor(&tx, &actor).await.unwrap();
    assert!(state.enabled);
    assert_eq!(state.currency.as_deref(), Some("INR"));
    assert_eq!(queries.lock().unwrap().len(), 1);
    assert!(queries.lock().unwrap()[0].contains("loan_employee_state"));
}
#[tokio::test]
async fn missing_or_wrong_tenant_metadata_fails_before_company_reads() {
    let tenant = Uuid::new_v4();
    let context = metadata(Uuid::new_v4(), true).await;
    assert!(actor(tenant).with_tenant_entitlements(context).is_err());
    let (db, queries) = company(false).await;
    let tx = db.begin().await.unwrap();
    assert!(payroll_loan_state_with_actor(&tx, &actor(tenant))
        .await
        .is_err());
    assert!(queries.lock().unwrap().is_empty());
}
#[tokio::test]
async fn disabled_loans_with_existing_balances_still_require_review() {
    let tenant = Uuid::new_v4();
    let actor = actor(tenant)
        .with_tenant_entitlements(metadata(tenant, false).await)
        .unwrap();
    let (db, _) = company(true).await;
    let tx = db.begin().await.unwrap();
    assert!(payroll_loan_state_with_actor(&tx, &actor).await.is_err());
}
