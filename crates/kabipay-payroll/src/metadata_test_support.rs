use kabipay_common::{
    context::{ClientClaims, CLIENT_JWT_ISSUER},
    entitlements::Entitlements,
};

use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::{
    collections::{BTreeMap, HashMap, VecDeque},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug)]
struct MetadataProxy(Mutex<VecDeque<(&'static str, Vec<ProxyRow>)>>);
#[async_trait::async_trait]
impl ProxyDatabaseTrait for MetadataProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let (table, rows) = self
            .0
            .lock()
            .unwrap()
            .pop_front()
            .ok_or_else(|| DbErr::Custom("unexpected metadata read".into()))?;
        assert!(
            statement.to_string().contains(table),
            "wrong metadata query for {table}"
        );
        Ok(rows)
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("metadata is read only")
    }
}

pub(crate) async fn ops(tenant: Uuid, enabled: bool) -> DatabaseConnection {
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
        ("\"tenant\"", vec![ProxyRow::new(fields)]),
        ("\"tenant_subscription\"", vec![]),
        ("\"feature_flag\"", vec![]),
        ("\"module\"", modules),
        ("kabipay_ops.module_dependency", vec![]),
    ])));
    let ops = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(proxy)))
        .await
        .unwrap();
    ops
}

pub(crate) fn claims(tenant: Uuid) -> ClientClaims {
    ClientClaims {
        sub: Uuid::new_v4(),
        tenant_id: tenant,
        iss: CLIENT_JWT_ISSUER.into(),
        exp: 0,
        iat: 0,
        email: String::new(),
        employee_id: None,
        must_change_password: false,
        roles: vec![],
        permissions: vec!["payroll:manage".into(), "onboarding:manage".into()],
        permission_scopes: HashMap::from([
            ("payroll:manage".into(), "ALL".into()),
            ("onboarding:manage".into(), "ALL".into()),
        ]),
        resource_scopes: HashMap::new(),
    }
}

pub(crate) async fn metadata(tenant: Uuid, enabled: bool) -> Entitlements {
    Entitlements::load(&ops(tenant, enabled).await, tenant)
        .await
        .unwrap()
}
