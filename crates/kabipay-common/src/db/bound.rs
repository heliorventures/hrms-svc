//! Fixed database configuration for a company runtime. No registry-selected DSN.
use super::{connect_tenant_pool_with_options, TenantDbCache, TenantDbHandle};
use crate::{KabiPayError, KabiPayResult};
use std::sync::Arc;
use uuid::Uuid;

pub struct BoundTenantDatabase {
    tenant_id: Uuid,
    options: sqlx::postgres::PgConnectOptions,
    schema: String,
    host: String,
    database: String,
}
impl std::fmt::Debug for BoundTenantDatabase {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("BoundTenantDatabase")
            .field("tenant_id", &self.tenant_id)
            .field("schema", &self.schema)
            .finish_non_exhaustive()
    }
}
impl BoundTenantDatabase {
    pub fn from_values(
        tenant: Option<&str>,
        url: Option<&str>,
        schema: Option<&str>,
    ) -> KabiPayResult<Option<Self>> {
        if tenant.is_none() && url.is_none() && schema.is_none() {
            return Ok(None);
        }
        let (Some(tenant), Some(url), Some(schema)) = (tenant, url, schema) else {
            return Err(configuration_error());
        };
        let tenant_id = Uuid::parse_str(tenant).map_err(|_| configuration_error())?;
        let mut chars = schema.chars();
        if tenant_id.is_nil()
            || schema.len() > 63
            || !chars
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
            || !chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
            || matches!(
                schema.to_ascii_lowercase().as_str(),
                "public" | "kabipay_ops" | "information_schema"
            )
            || schema.to_ascii_lowercase().starts_with("pg_")
            || !(url.starts_with("postgres://") || url.starts_with("postgresql://"))
        {
            return Err(configuration_error());
        }
        let parsed = reqwest::Url::parse(url).map_err(|_| configuration_error())?;
        if parsed.host_str().is_none()
            || parsed.username().is_empty()
            || parsed.path().trim_start_matches('/').is_empty()
            || parsed.fragment().is_some()
            || parsed.query_pairs().any(|(key, _)| {
                !matches!(
                    key.as_ref(),
                    "sslmode"
                        | "ssl-mode"
                        | "sslrootcert"
                        | "ssl-root-cert"
                        | "ssl-ca"
                        | "sslcert"
                        | "ssl-cert"
                        | "sslkey"
                        | "ssl-key"
                        | "statement-cache-capacity"
                        | "application_name"
                )
            })
        {
            return Err(configuration_error());
        }
        // Parse once at startup. Reconnection must not reinterpret later process
        // environment changes or let PGPORT replace the explicit URL's default.
        let opts = url
            .parse::<sqlx::postgres::PgConnectOptions>()
            .map_err(|_| configuration_error())?
            .port(parsed.port().unwrap_or(5432));
        let database = opts
            .get_database()
            .filter(|name| !name.is_empty())
            .ok_or_else(configuration_error)?;
        Ok(Some(Self {
            tenant_id,
            schema: schema.to_owned(),
            host: opts.get_host().to_owned(),
            database: database.to_owned(),
            options: opts,
        }))
    }
    fn matches_mapping(&self, host: &str, database: &str, schema: &str) -> bool {
        self.host == host && self.database == database && self.schema == schema
    }
}
fn configuration_error() -> KabiPayError {
    KabiPayError::Internal("invalid bound database configuration".into())
}
impl TenantDbCache {
    pub async fn validate_binding_registration(
        &self,
        ops: &sea_orm::DatabaseConnection,
    ) -> KabiPayResult<()> {
        use kabipay_db_entities::ops::tenant_database;
        use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
        let Some(binding) = &self.binding else {
            return Ok(());
        };
        let rows = tenant_database::Entity::find()
            .filter(tenant_database::Column::TenantId.eq(binding.tenant_id))
            .filter(tenant_database::Column::IsActive.eq(true))
            .all(ops)
            .await?;
        if rows.len() != 1
            || !rows.first().is_some_and(|row| {
                row.db_type == "POSTGRES"
                    && binding.matches_mapping(&row.db_host, &row.db_name, &row.schema_name)
            })
        {
            return Err(KabiPayError::TenantDatabaseUnavailable(binding.tenant_id));
        }
        Ok(())
    }
    pub fn with_binding(binding: BoundTenantDatabase) -> Self {
        Self {
            binding: Some(Arc::new(binding)),
            ..Self::default()
        }
    }
    pub fn from_env() -> KabiPayResult<Self> {
        crate::client_jwt::validate_client_token_configuration(false)?;
        let values = [
            "KABIPAY_BOUND_TENANT_ID",
            "KABIPAY_BOUND_DATABASE_URL",
            "KABIPAY_BOUND_SCHEMA",
        ]
        .map(|name| match std::env::var(name) {
            Ok(value) => Ok(Some(value)),
            Err(std::env::VarError::NotPresent) => Ok(None),
            Err(std::env::VarError::NotUnicode(_)) => Err(configuration_error()),
        });
        let [tenant, url, schema] = values;
        let (tenant, url, schema) = (tenant?, url?, schema?);
        Ok(
            match BoundTenantDatabase::from_values(
                tenant.as_deref(),
                url.as_deref(),
                schema.as_deref(),
            )? {
                Some(binding) => Self::with_binding(binding),
                None => Self::new(),
            },
        )
    }
    pub fn bound_tenant_id(&self) -> Option<Uuid> {
        self.binding.as_ref().map(|b| b.tenant_id)
    }
    pub fn require_tenant(&self, tenant: Uuid) -> KabiPayResult<()> {
        if self
            .bound_tenant_id()
            .is_some_and(|expected| expected != tenant)
        {
            return Err(KabiPayError::Forbidden(
                "company runtime scope mismatch".into(),
            ));
        }
        Ok(())
    }
    pub(super) async fn resolve_bound(&self, tenant_id: Uuid) -> KabiPayResult<TenantDbHandle> {
        self.require_tenant(tenant_id)?;
        let binding = self
            .binding
            .as_ref()
            .ok_or_else(|| KabiPayError::Internal("bound runtime is not configured".into()))?;
        let _guard = self.bound_initialization.lock().await;
        if let Some(handle) = self.inner.get(&tenant_id) {
            if !handle.from_ops_row
                && binding.matches_mapping(&handle.db_host, &handle.db_name, &handle.schema_name)
            {
                return Ok(handle.clone());
            }
        }
        self.inner.remove(&tenant_id);
        let conn = connect_tenant_pool_with_options(binding.options.clone(), &binding.schema)
            .await
            .map_err(|_| KabiPayError::TenantDatabaseUnavailable(tenant_id))?;
        let handle = TenantDbHandle {
            conn,
            schema_name: binding.schema.clone(),
            db_host: binding.host.clone(),
            db_name: binding.database.clone(),
            from_ops_row: false,
        };
        self.inner.insert(tenant_id, handle.clone());
        Ok(handle)
    }
}

#[cfg(test)]
#[path = "bound_tests.rs"]
mod tests;
