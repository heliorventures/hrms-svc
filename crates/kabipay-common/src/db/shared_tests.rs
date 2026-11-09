use super::*;
use sea_orm::entity::prelude::async_trait;
use sea_orm::{
    Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow,
    Statement,
};

fn company(id: u128) -> serde_json::Value {
    serde_json::json!({"tenant_id":Uuid::from_u128(id),
        "database_url":format!("postgres://company_{id}:synthetic@localhost:1/company_{id}?sslmode=verify-full"),
        "schema":format!("tenant_{id}"),"public_origin":format!("https://company-{id}.example.com"),"settings":{}})
}
fn parse(companies: Vec<serde_json::Value>, budget: u32) -> KabiPayResult<SharedCompanies> {
    SharedCompanies::parse(
        &serde_json::to_vec(&serde_json::json!({"version":1,"companies":companies})).unwrap(),
        budget,
        1,
    )
}
fn cache() -> TenantDbCache {
    let config = parse(vec![company(1), company(2)], 2).unwrap();
    let cache = TenantDbCache {
        shared: Some(Arc::new(config)),
        ..TenantDbCache::default()
    };
    for n in [1, 2] {
        cache.inner.insert(
            Uuid::from_u128(n),
            TenantDbHandle {
                conn: DatabaseConnection::Disconnected,
                schema_name: format!("tenant_{n}"),
                db_host: "localhost".into(),
                db_name: format!("company_{n}"),
                from_ops_row: true,
            },
        );
    }
    cache
}
#[test]
fn shared_configuration_rejects_duplicate_companies_databases_and_credentials() {
    assert!(parse(vec![company(1), company(2)], 2).is_ok());
    assert!(parse(vec![company(1), company(1)], 2).is_err());
    let mut same_db = company(2);
    same_db["database_url"] = serde_json::json!(
        "postgres://company_2:synthetic@localhost:1/company_1?sslmode=verify-full"
    );
    assert!(parse(vec![company(1), same_db], 2).is_err());
    let mut same_user = company(2);
    same_user["database_url"] = serde_json::json!(
        "postgres://company_1:synthetic@localhost:1/company_2?sslmode=verify-full"
    );
    assert!(parse(vec![company(1), same_user], 2).is_err());
}
#[test]
fn shared_configuration_rejects_unverified_tls_unsafe_settings_and_excess_pools() {
    assert!(parse(vec![company(1), company(2)], 1).is_err());
    for (field, value) in [
        (
            "database_url",
            serde_json::json!("postgres://u:p@localhost/db?sslmode=require"),
        ),
        (
            "database_url",
            serde_json::json!("postgres://u:p@localhost/db?sslmode=verify-full&sslmode=disable"),
        ),
        ("schema", serde_json::json!("kabipay_ops")),
        (
            "public_origin",
            serde_json::json!("https://company.example.com/other"),
        ),
        ("settings", serde_json::json!({"DATABASE_URL":"unexpected"})),
        (
            "settings",
            serde_json::json!({"KABIPAY_PRIVATE_EVENT_KEYS_FILE":"relative/key.json"}),
        ),
        (
            "settings",
            serde_json::json!({"KABIPAY_PRIVATE_EVENT_KEYS_FILE":"/run/../key.json"}),
        ),
    ] {
        let mut entry = company(1);
        entry[field] = value;
        assert!(parse(vec![entry], 1).is_err(), "{field}");
    }
}
#[test]
fn shared_routing_requires_a_configured_candidate_without_enabling_legacy_fallback() {
    let cache = cache();
    let mut headers = axum::http::HeaderMap::new();
    assert!(cache.requested_tenant(&headers).is_err());
    headers.insert(
        "x-tenant-id",
        Uuid::from_u128(1).to_string().parse().unwrap(),
    );
    assert!(cache.requested_tenant(&headers).is_err());
    headers.insert(
        "x-organization-company-id",
        Uuid::from_u128(1).to_string().parse().unwrap(),
    );
    assert_eq!(
        cache.requested_tenant(&headers).unwrap(),
        Uuid::from_u128(1)
    );
    headers.insert(
        "x-organization-company-id",
        Uuid::from_u128(3).to_string().parse().unwrap(),
    );
    assert!(cache.requested_tenant(&headers).is_err());
    assert!(cache.require_tenant(Uuid::from_u128(3)).is_err());
}
#[derive(Debug)]
struct Registry {
    tenant: Uuid,
    status: &'static str,
    database: &'static str,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Registry {
    async fn query(&self, sql: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = sql.to_string();
        assert!(sql.contains(&self.tenant.to_string()));
        let now = chrono::Utc::now();
        let mut fields = BTreeMap::from([
            ("id".into(), self.tenant.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]);
        if sql.contains("tenant_database") {
            fields.extend([
                ("tenant_id".into(), self.tenant.into()),
                ("db_type".into(), "POSTGRES".into()),
                ("db_host".into(), "localhost".into()),
                ("db_name".into(), self.database.into()),
                (
                    "schema_name".into(),
                    format!("tenant_{}", self.tenant.as_u128()).into(),
                ),
                ("is_active".into(), true.into()),
            ]);
        } else {
            fields.extend([
                ("name".into(), "fixture".into()),
                ("status".into(), self.status.into()),
                ("is_deleted".into(), false.into()),
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
        }
        Ok(vec![ProxyRow::new(fields)])
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("routing must not write")
    }
}
async fn registry(
    tenant: u128,
    status: &'static str,
    database: &'static str,
) -> DatabaseConnection {
    Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Registry {
            tenant: Uuid::from_u128(tenant),
            status,
            database,
        })),
    )
    .await
    .unwrap()
}
#[tokio::test]
async fn cached_pool_is_rejected_after_company_suspension_or_registry_remapping() {
    for (status, database) in [("SUSPENDED", "company_1"), ("ACTIVE", "changed_database")] {
        let cache = cache();
        assert!(cache
            .resolve_shared(Uuid::from_u128(1), &registry(1, status, database).await)
            .await
            .is_err());
        assert!(!cache.inner.contains_key(&Uuid::from_u128(1)));
    }
}
#[tokio::test]
async fn another_company_initializing_does_not_block_a_healthy_cached_company() {
    let cache = cache();
    let config = cache.shared.as_ref().unwrap();
    let _busy = config.companies[&Uuid::from_u128(1)]
        .initialization
        .lock()
        .await;
    let db = registry(2, "ACTIVE", "company_2").await;
    let result = tokio::time::timeout(
        std::time::Duration::from_millis(200),
        cache.resolve_shared(Uuid::from_u128(2), &db),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result.db_name, "company_2");
}
#[tokio::test]
async fn unknown_company_is_rejected_before_any_registry_or_database_access() {
    assert!(matches!(
        cache()
            .resolve_shared(Uuid::from_u128(3), &DatabaseConnection::Disconnected)
            .await,
        Err(KabiPayError::TenantDatabaseUnavailable(_))
    ));
}
