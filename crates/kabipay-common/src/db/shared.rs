//! Immutable deployment allowlist for shared company application processes.
//! Database secrets never come from request headers or registry values.
use super::{
    bound::BoundTenantDatabase,
    connect_tenant_pool_with_options,
    registry::{Mapping, RegistryDatabase},
    TenantDbCache, TenantDbHandle,
};
use crate::{KabiPayError, KabiPayResult};
use serde::Deserialize;
use std::{
    collections::{BTreeMap, HashSet},
    io::Read,
    path::{Component, Path},
    sync::{Arc, OnceLock},
};
use uuid::Uuid;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    companies: Vec<CompanyConfig>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CompanyConfig {
    tenant_id: Uuid,
    database_url: Option<String>,
    schema: Option<String>,
    public_origin: String,
    #[serde(default)]
    settings: BTreeMap<String, String>,
}
pub(super) struct Company {
    binding: Option<BoundTenantDatabase>,
    pinned: OnceLock<Mapping>,
    initialization: tokio::sync::Mutex<()>,
    origin: String,
    settings: BTreeMap<String, String>,
}
pub(super) struct SharedCompanies {
    companies: BTreeMap<Uuid, Company>,
    registry: Option<RegistryDatabase>,
}
fn invalid() -> KabiPayError {
    KabiPayError::Internal("invalid shared company configuration".into())
}
fn allowed_setting(name: &str) -> bool {
    matches!(
        name,
        "KABIPAY_CLIENT_JWT_KEY_ID"
            | "KABIPAY_CLIENT_JWT_PRIVATE_KEY_FILE"
            | "KABIPAY_PRIVATE_EVENT_KEYS_FILE"
            | "KABIPAY_PRIVATE_HISTORY_CLIENT_FILE"
            | "KABIPAY_PRIVATE_HISTORY_CLIENT_CERT_FILE"
            | "KABIPAY_PRIVATE_HISTORY_CLIENT_KEY_FILE"
            | "KABIPAY_PRIVATE_DELIVERY_HISTORY_CLIENT_FILE"
            | "KABIPAY_PRIVATE_DELIVERY_ADAPTER_FILE"
            | "KABIPAY_PRIVATE_DELIVERY_BROKER_OWNER_ID"
    )
}
impl SharedCompanies {
    #[cfg(test)]
    fn parse(bytes: &[u8], budget: u32, per_company: u32) -> KabiPayResult<Self> {
        Self::parse_with_registry(bytes, budget, per_company, None)
    }
    fn parse_with_registry(
        bytes: &[u8],
        budget: u32,
        per_company: u32,
        registry: Option<RegistryDatabase>,
    ) -> KabiPayResult<Self> {
        let config: Config = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if !matches!((config.version, registry.is_some()), (1, false) | (2, true))
            || config.companies.is_empty()
            || config.companies.len() > 100
        {
            return Err(invalid());
        }
        if per_company == 0
            || (config.companies.len() as u32)
                .checked_mul(per_company)
                .is_none_or(|total| total > budget)
        {
            return Err(invalid());
        }
        let mut companies = BTreeMap::new();
        let mut databases = HashSet::new();
        let mut users = HashSet::new();
        for company in config.companies {
            if company.tenant_id.is_nil() {
                return Err(invalid());
            }
            let binding = if config.version == 2 {
                if company.database_url.is_some() || company.schema.is_some() {
                    return Err(invalid());
                }
                None
            } else {
                let database_url = company.database_url.as_deref().ok_or_else(invalid)?;
                let url = reqwest::Url::parse(database_url).map_err(|_| invalid())?;
                if url.password().is_none()
                    || !url
                        .query_pairs()
                        .any(|(key, value)| key == "sslmode" && value == "verify-full")
                {
                    return Err(invalid());
                }
                let binding = BoundTenantDatabase::from_values(
                    Some(&company.tenant_id.to_string()),
                    Some(database_url),
                    company.schema.as_deref(),
                )?
                .ok_or_else(invalid)?;
                if !matches!(
                    binding.options.get_ssl_mode(),
                    sqlx::postgres::PgSslMode::VerifyFull
                ) {
                    return Err(invalid());
                }
                // Two company schemas in one database are not this deployment model.
                if !databases.insert((
                    binding.host.clone(),
                    binding.options.get_port(),
                    binding.database.clone(),
                )) {
                    return Err(invalid());
                }
                if !users.insert((
                    binding.host.clone(),
                    binding.options.get_port(),
                    binding.options.get_username().to_owned(),
                )) {
                    return Err(invalid());
                }
                Some(binding)
            };
            let origin = reqwest::Url::parse(&company.public_origin).map_err(|_| invalid())?;
            if origin.scheme() != "https"
                || origin.host_str().is_none()
                || !origin.username().is_empty()
                || origin.password().is_some()
                || origin.path() != "/"
                || origin.query().is_some()
                || origin.fragment().is_some()
            {
                return Err(invalid());
            }
            for (name, value) in &company.settings {
                if !allowed_setting(name) || value.is_empty() || value.len() > 4096 {
                    return Err(invalid());
                }
                if name.ends_with("_FILE")
                    && (!Path::new(value).is_absolute()
                        || Path::new(value)
                            .components()
                            .any(|part| matches!(part, Component::ParentDir)))
                {
                    return Err(invalid());
                }
            }
            if companies
                .insert(
                    company.tenant_id,
                    Company {
                        binding,
                        pinned: OnceLock::new(),
                        initialization: tokio::sync::Mutex::new(()),
                        origin: company.public_origin.trim_end_matches('/').to_owned(),
                        settings: company.settings,
                    },
                )
                .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self {
            companies,
            registry,
        })
    }
}
static CONFIG: OnceLock<Result<Option<Arc<SharedCompanies>>, ()>> = OnceLock::new();
pub(super) fn configured() -> KabiPayResult<Option<Arc<SharedCompanies>>> {
    CONFIG
        .get_or_init(|| {
            let Some(path) = std::env::var_os("KABIPAY_COMPANY_CONFIG_FILE") else {
                if std::env::var_os("KABIPAY_COMPANY_DATABASE_FILE").is_some() {
                    return Err(());
                }
                return Ok(None);
            };
            let read = || -> KabiPayResult<Arc<SharedCompanies>> {
                if !Path::new(&path).is_absolute() {
                    return Err(invalid());
                }
                let file = std::fs::File::open(path).map_err(|_| invalid())?;
                if !file.metadata().map_err(|_| invalid())?.is_file() {
                    return Err(invalid());
                }
                let mut bytes = Vec::new();
                file.take(1_048_577)
                    .read_to_end(&mut bytes)
                    .map_err(|_| invalid())?;
                if bytes.len() > 1_048_576 {
                    return Err(invalid());
                }
                let budget = std::env::var("KABIPAY_TENANT_DB_CONNECTION_BUDGET")
                    .map_err(|_| invalid())?
                    .parse()
                    .map_err(|_| invalid())?;
                let registry = std::env::var_os("KABIPAY_COMPANY_DATABASE_FILE")
                    .map(|path| RegistryDatabase::from_file(Path::new(&path)))
                    .transpose()?;
                Ok(Arc::new(SharedCompanies::parse_with_registry(
                    &bytes,
                    budget,
                    super::tenant_pool_max_connections(),
                    registry,
                )?))
            };
            read().map(Some).map_err(|_| ())
        })
        .as_ref()
        .map(Clone::clone)
        .map_err(|_| invalid())
}

impl Company {
    async fn database(
        &self,
        tenant: Uuid,
        registry: Option<&RegistryDatabase>,
        ops: &sea_orm::DatabaseConnection,
    ) -> KabiPayResult<BoundTenantDatabase> {
        if let Some(binding) = &self.binding {
            binding.validate_registration(ops).await?;
            return Ok(binding.clone());
        }
        let binding = registry.ok_or_else(invalid)?.resolve(tenant, ops).await?;
        let mapping = Mapping {
            database: binding.database.clone(),
            schema: binding.schema.clone(),
        };
        if self.pinned.get_or_init(|| mapping.clone()) != &mapping {
            return Err(KabiPayError::TenantDatabaseUnavailable(tenant));
        }
        Ok(binding)
    }
}

#[cfg(test)]
#[path = "shared_tests.rs"]
mod tests;
/// A configured per-company secret reference, or the legacy process setting.
/// Shared mode never falls back when a company or its setting is missing.
pub fn company_setting(tenant: Uuid, name: &str) -> KabiPayResult<String> {
    if !allowed_setting(name) {
        return Err(invalid());
    }
    match configured()? {
        Some(config) => config
            .companies
            .get(&tenant)
            .and_then(|c| c.settings.get(name))
            .cloned()
            .ok_or_else(invalid),
        None => std::env::var(name).map_err(|_| invalid()),
    }
}
pub fn company_public_origin(tenant: Uuid) -> KabiPayResult<Option<String>> {
    configured()?
        .map(|config| {
            config
                .companies
                .get(&tenant)
                .map(|c| c.origin.clone())
                .ok_or_else(invalid)
        })
        .transpose()
}
pub fn shared_company_ids() -> KabiPayResult<Option<Vec<Uuid>>> {
    Ok(configured()?.map(|config| config.companies.keys().copied().collect()))
}
impl TenantDbCache {
    /// Select an allowlisted routing candidate. The caller MUST verify a signed
    /// credential for this exact company before accessing its database.
    pub fn requested_tenant(&self, headers: &axum::http::HeaderMap) -> KabiPayResult<Uuid> {
        let supplied = headers
            .get("x-organization-company-id")
            .map(|value| {
                value
                    .to_str()
                    .ok()
                    .and_then(|value| value.parse::<Uuid>().ok())
                    .ok_or(KabiPayError::Unauthorised)
            })
            .transpose()?;
        let tenant = match (self.bound_tenant_id(), supplied) {
            (Some(bound), None) => bound,
            (Some(bound), Some(id)) if id == bound => id,
            (None, Some(id)) if self.shared.is_some() => id,
            _ => return Err(KabiPayError::Unauthorised),
        };
        self.require_tenant(tenant)?;
        Ok(tenant)
    }
    /// Explicitly configured companies only; no registry enumeration or fallback.
    pub fn configured_tenant_ids(&self) -> Vec<Uuid> {
        self.shared
            .as_ref()
            .map(|c| c.companies.keys().copied().collect())
            .unwrap_or_else(|| self.bound_tenant_id().into_iter().collect())
    }
    pub fn is_company_runtime(&self) -> bool {
        self.shared.is_some() || self.binding.is_some()
    }
    pub(super) fn shared_contains(&self, tenant: Uuid) -> bool {
        self.shared
            .as_ref()
            .is_none_or(|c| c.companies.contains_key(&tenant))
    }
    pub(super) async fn validate_shared_registration(
        &self,
        ops: &sea_orm::DatabaseConnection,
    ) -> KabiPayResult<()> {
        if let Some(config) = &self.shared {
            for (tenant, company) in &config.companies {
                company
                    .database(*tenant, config.registry.as_ref(), ops)
                    .await?;
            }
        }
        Ok(())
    }
    pub(super) async fn resolve_shared(
        &self,
        tenant: Uuid,
        ops: &sea_orm::DatabaseConnection,
    ) -> KabiPayResult<TenantDbHandle> {
        use sea_orm::EntityTrait;
        let company = self
            .shared
            .as_ref()
            .and_then(|c| c.companies.get(&tenant))
            .ok_or(KabiPayError::TenantDatabaseUnavailable(tenant))?;
        // Serialize first creation and registry validation: no duplicate pools or
        // stale mapping can be admitted through the cached fast path.
        let _guard = company.initialization.lock().await;
        let active = kabipay_db_entities::ops::tenant::Entity::find_by_id(tenant)
            .one(ops)
            .await?
            .is_some_and(|row| row.status == "ACTIVE" && !row.is_deleted);
        let binding = if active {
            company
                .database(
                    tenant,
                    self.shared.as_ref().and_then(|c| c.registry.as_ref()),
                    ops,
                )
                .await
        } else {
            Err(KabiPayError::TenantDatabaseUnavailable(tenant))
        };
        let binding = match binding {
            Ok(binding) => binding,
            Err(_) => {
                if let Some((_, handle)) = self.inner.remove(&tenant) {
                    // Existing in-flight work must drain without holding the cache
                    // creation lock, which that work may itself need for another DB.
                    tokio::spawn(async move {
                        let _ = handle.conn.close().await;
                    });
                }
                return Err(KabiPayError::TenantDatabaseUnavailable(tenant));
            }
        };
        if let Some(handle) = self.inner.get(&tenant) {
            if handle.db_host == binding.host
                && handle.db_name == binding.database
                && handle.schema_name == binding.schema
            {
                return Ok(handle.clone());
            }
            return Err(KabiPayError::TenantDatabaseUnavailable(tenant));
        }
        let conn = connect_tenant_pool_with_options(binding.options.clone(), &binding.schema)
            .await
            .map_err(|_| KabiPayError::TenantDatabaseUnavailable(tenant))?;
        let handle = TenantDbHandle {
            conn,
            schema_name: binding.schema.clone(),
            db_host: binding.host.clone(),
            db_name: binding.database.clone(),
            from_ops_row: true,
        };
        self.inner.insert(tenant, handle.clone());
        Ok(handle)
    }
}
