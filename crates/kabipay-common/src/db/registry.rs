//! Company routing from trusted metadata, using fixed service credentials.
use super::bound::BoundTenantDatabase;
use crate::{KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::tenant_database;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use serde::Deserialize;
use std::{io::Read, path::Path};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    server_url: String,
    database_prefix: String,
}
pub(super) struct RegistryDatabase {
    options: sqlx::postgres::PgConnectOptions,
    host: String,
    prefix: String,
}
#[derive(Clone, PartialEq, Eq)]
pub(super) struct Mapping {
    pub(super) database: String,
    pub(super) schema: String,
}
fn invalid() -> KabiPayError {
    KabiPayError::Internal("invalid company database registry configuration".into())
}
fn identifier(value: &str) -> bool {
    let mut chars = value.chars();
    value.len() <= 63
        && chars
            .next()
            .is_some_and(|c| c.is_ascii_lowercase() || c == '_')
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '_')
        && !matches!(
            value,
            "public"
                | "postgres"
                | "template0"
                | "template1"
                | "kabipay_ops"
                | "information_schema"
        )
        && !value.starts_with("pg_")
}
impl RegistryDatabase {
    pub(super) fn from_file(path: &Path) -> KabiPayResult<Self> {
        if !path.is_absolute() {
            return Err(invalid());
        }
        let file = std::fs::File::open(path).map_err(|_| invalid())?;
        if !file.metadata().map_err(|_| invalid())?.is_file() {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(16_385)
            .read_to_end(&mut bytes)
            .map_err(|_| invalid())?;
        if bytes.len() > 16_384 {
            return Err(invalid());
        }
        Self::parse(&bytes)
    }
    pub(super) fn parse(bytes: &[u8]) -> KabiPayResult<Self> {
        let config: Config = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if config.version != 1
            || !identifier(&config.database_prefix)
            || !(2..=32).contains(&config.database_prefix.len())
            || !config.database_prefix.ends_with('_')
        {
            return Err(invalid());
        }
        let url = reqwest::Url::parse(&config.server_url).map_err(|_| invalid())?;
        let modes: Vec<_> = url
            .query_pairs()
            .filter(|(k, _)| k == "sslmode" || k == "ssl-mode")
            .collect();
        if url.password().is_none_or(str::is_empty)
            || modes.len() != 1
            || modes[0].0 != "sslmode"
            || modes[0].1 != "verify-full"
        {
            return Err(invalid());
        }
        let binding = BoundTenantDatabase::from_values(
            Some(&Uuid::from_u128(1).to_string()),
            Some(&config.server_url),
            Some("tenant_runtime"),
        )?
        .ok_or_else(invalid)?;
        if !matches!(
            binding.options.get_ssl_mode(),
            sqlx::postgres::PgSslMode::VerifyFull
        ) || binding.options.get_username() == "postgres"
        {
            return Err(invalid());
        }
        Ok(Self {
            options: binding.options,
            host: binding.host,
            prefix: config.database_prefix,
        })
    }
    pub(super) fn bind(
        &self,
        row: &tenant_database::Model,
        tenant: Uuid,
    ) -> KabiPayResult<BoundTenantDatabase> {
        if row.tenant_id != tenant
            || !row.is_active
            || row.db_type != "POSTGRES"
            || row.db_host != self.host
            || !identifier(&row.db_name)
            || !identifier(&row.schema_name)
            || !row.db_name.starts_with(&self.prefix)
            || row.db_name.len() <= self.prefix.len()
        {
            return Err(KabiPayError::TenantDatabaseUnavailable(tenant));
        }
        Ok(BoundTenantDatabase {
            tenant_id: tenant,
            options: self.options.clone().database(&row.db_name),
            host: self.host.clone(),
            database: row.db_name.clone(),
            schema: row.schema_name.clone(),
        })
    }
    pub(super) async fn resolve(
        &self,
        tenant: Uuid,
        ops: &DatabaseConnection,
    ) -> KabiPayResult<BoundTenantDatabase> {
        let rows = tenant_database::Entity::find()
            .filter(tenant_database::Column::TenantId.eq(tenant))
            .filter(tenant_database::Column::IsActive.eq(true))
            .all(ops)
            .await?;
        if rows.len() != 1 {
            return Err(KabiPayError::TenantDatabaseUnavailable(tenant));
        }
        let binding = self.bind(&rows[0], tenant)?;
        // A service login can access several databases: reject a registry alias
        // that would make two companies share one database, even different schemas.
        let owners = tenant_database::Entity::find()
            .filter(tenant_database::Column::DbName.eq(&binding.database))
            .filter(tenant_database::Column::IsActive.eq(true))
            .all(ops)
            .await?;
        if owners.len() != 1 || owners[0].tenant_id != tenant {
            return Err(KabiPayError::TenantDatabaseUnavailable(tenant));
        }
        Ok(binding)
    }
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod tests;
