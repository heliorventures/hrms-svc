use super::*;

fn config(url: &str, prefix: &str) -> KabiPayResult<RegistryDatabase> {
    RegistryDatabase::parse(
        &serde_json::to_vec(&serde_json::json!({
            "version":1,"server_url":url,"database_prefix":prefix,
        }))
        .unwrap(),
    )
}
fn row(id: u128) -> tenant_database::Model {
    tenant_database::Model {
        id: Uuid::from_u128(id),
        tenant_id: Uuid::from_u128(id),
        db_type: "POSTGRES".into(),
        db_host: "postgres".into(),
        db_name: format!("company_{id}"),
        schema_name: format!("tenant_{id}"),
        is_active: true,
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}
const URL: &str =
    "postgres://employee_runtime:synthetic@postgres:5432/postgres?sslmode=verify-full";

#[test]
fn one_service_login_is_reused_across_separate_registry_databases() {
    let config = config(URL, "company_").unwrap();
    for id in [1, 2, 3] {
        let binding = config.bind(&row(id), Uuid::from_u128(id)).unwrap();
        assert_eq!(binding.options.get_username(), "employee_runtime");
        assert_eq!(binding.options.get_host(), "postgres");
        assert_eq!(binding.options.get_port(), 5432);
        assert_eq!(
            binding.options.get_database(),
            Some(format!("company_{id}").as_str())
        );
        assert!(matches!(
            binding.options.get_ssl_mode(),
            sqlx::postgres::PgSslMode::VerifyFull
        ));
    }
}
#[test]
fn registry_cannot_redirect_credentials_or_select_unsafe_databases_and_schemas() {
    let config = config(URL, "company_").unwrap();
    for field in ["host", "database", "schema", "type", "tenant", "active"] {
        let mut value = row(1);
        match field {
            "host" => value.db_host = "evil.example".into(),
            "database" => value.db_name = "helior".into(),
            "schema" => value.schema_name = "kabipay_ops".into(),
            "type" => value.db_type = "MONGO".into(),
            "tenant" => value.tenant_id = Uuid::from_u128(2),
            _ => value.is_active = false,
        }
        assert!(config.bind(&value, Uuid::from_u128(1)).is_err(), "{field}");
    }
    for name in [
        "company_",
        "company_a?sslmode=disable",
        "company_a/other",
        "company_a;DROP",
        "company_../x",
    ] {
        let mut value = row(1);
        value.db_name = name.into();
        assert!(config.bind(&value, value.tenant_id).is_err());
    }
}
#[test]
fn service_configuration_rejects_missing_password_admin_login_tls_downgrade_and_broad_prefix() {
    for url in [
        URL.replace(":synthetic", ""),
        URL.replace("employee_runtime", "postgres"),
        URL.replace("verify-full", "require"),
        format!("{URL}&sslmode=disable"),
        format!("{URL}&ssl-mode=disable"),
    ] {
        assert!(config(&url, "company_").is_err());
    }
    for prefix in ["", "company", "pg_", "../", "Company_"] {
        assert!(config(URL, prefix).is_err());
    }
}

#[derive(Debug)]
struct RegistryRows {
    mappings: usize,
    owners: usize,
}
#[sea_orm::entity::prelude::async_trait::async_trait]
impl sea_orm::ProxyDatabaseTrait for RegistryRows {
    async fn query(
        &self,
        statement: sea_orm::Statement,
    ) -> Result<Vec<sea_orm::ProxyRow>, sea_orm::DbErr> {
        let sql = statement.to_string();
        let count = if sql.contains("\"tenant_id\" =") {
            self.mappings
        } else {
            self.owners
        };
        Ok((0..count)
            .map(|index| {
                let model = row(index as u128 + 1);
                sea_orm::ProxyRow::new(std::collections::BTreeMap::from([
                    ("id".into(), model.id.into()),
                    ("tenant_id".into(), model.tenant_id.into()),
                    ("db_type".into(), model.db_type.into()),
                    ("db_host".into(), model.db_host.into()),
                    ("db_name".into(), "company_1".into()),
                    ("schema_name".into(), model.schema_name.into()),
                    ("is_active".into(), true.into()),
                    ("created_at".into(), model.created_at.into()),
                    ("updated_at".into(), model.updated_at.into()),
                ]))
            })
            .collect())
    }
    async fn execute(
        &self,
        _: sea_orm::Statement,
    ) -> Result<sea_orm::ProxyExecResult, sea_orm::DbErr> {
        panic!("routing must not write")
    }
}
#[tokio::test]
async fn registry_rejects_missing_duplicate_and_shared_database_mappings() {
    for (mappings, owners, accepted) in [
        (1, 1, true),
        (0, 1, false),
        (2, 1, false),
        (1, 2, false),
        (1, 0, false),
    ] {
        let db = sea_orm::Database::connect_proxy(
            sea_orm::DbBackend::Postgres,
            std::sync::Arc::new(Box::new(RegistryRows { mappings, owners })),
        )
        .await
        .unwrap();
        assert_eq!(
            config(URL, "company_")
                .unwrap()
                .resolve(Uuid::from_u128(1), &db)
                .await
                .is_ok(),
            accepted
        );
    }
}
