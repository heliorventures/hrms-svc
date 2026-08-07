//! Tenant database resolver.
//!
//! Each tenant lives in an isolated PostgreSQL schema. At request time we
//! resolve the tenant's schema from `kabipay_ops.tenant_database` and return
//! a SeaORM connection with `search_path` pinned to that schema.
//!
//! Connections are pooled and cached in [`TenantDbCache`]. Do not create a
//! fresh pool per request — it will exhaust the PostgreSQL connection limit.
//!
//! Tenant pools set `search_path` on **every checkout**: `after_connect` for new
//! physical connections plus `before_acquire` when handing out an idle connection.
//! Transaction-mode poolers (Neon pooler, PgBouncer) can discard session state between
//! uses; relying solely on SeaORM's `after_connect` hook causes `relation "user" does not exist`
//! because `public` has no tenant-plane tables.

use crate::error::{KabiPayError, KabiPayResult};
use dashmap::DashMap;
use kabipay_db_entities::ops::tenant_database;
use sea_orm::{
    ColumnTrait, ConnectOptions, Database, DatabaseConnection, EntityTrait, QueryFilter,
    SqlxPostgresConnector,
};
use sqlx::postgres::PgPoolOptions;
use sqlx::Connection;
use sqlx::ConnectOptions as SqlxConnectOptionsTrait;
use std::sync::Arc;
use std::time::Duration;
use uuid::Uuid;

fn pool_max_connections(fallback: u32) -> u32 {
    std::env::var("KABIPAY_DB_POOL_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(fallback)
}

fn pool_connect_timeout() -> Duration {
    let secs: u64 = std::env::var("KABIPAY_DB_CONNECT_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(30);
    Duration::from_secs(secs.max(1))
}

fn pool_acquire_timeout() -> Duration {
    let secs: u64 = std::env::var("KABIPAY_DB_ACQUIRE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(secs.max(1))
}

/// How long a pooled connection may sit unused before the pool closes it (ops + tenant pools).
/// Default 60s — shorter idle helps serverless / Neon compute go idle sooner.
fn pool_idle_timeout() -> Duration {
    let secs: u64 = std::env::var("KABIPAY_DB_IDLE_TIMEOUT_SECS")
        .ok()
        .and_then(|s| s.parse().ok())
        .unwrap_or(60);
    Duration::from_secs(secs.max(1))
}

fn tenant_pool_max_connections() -> u32 {
    std::env::var("KABIPAY_TENANT_DB_POOL_MAX")
        .ok()
        .and_then(|s| s.parse().ok())
        .filter(|&n| n > 0)
        .unwrap_or(2)
}

/// Resolved view of a single tenant's database. Kept around the pooled
/// connection so resolvers can query the caller's schema name without
/// re-hitting `kabipay_ops.tenant_database`.
#[derive(Clone, Debug)]
pub struct TenantDbHandle {
    pub conn: DatabaseConnection,
    pub schema_name: String,
    pub db_host: String,
    pub db_name: String,
    /// `true` when the handle was built from a real ops row; `false` when
    /// the scaffold fallback (`tenant_<hex[:8]>`) had to be used.
    pub from_ops_row: bool,
}

/// Cache of tenant_id -> pooled SeaORM connection + resolved metadata.
/// One entry per tenant, shared across all services in a process.
#[derive(Clone, Default)]
pub struct TenantDbCache {
    inner: Arc<DashMap<Uuid, TenantDbHandle>>,
}

impl TenantDbCache {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn len(&self) -> usize {
        self.inner.len()
    }

    pub fn is_empty(&self) -> bool {
        self.inner.is_empty()
    }

    /// Drop any cached connection for this tenant. Useful when the ops plane
    /// mutates `tenant_database` (e.g. tenant re-provisioning).
    pub fn invalidate(&self, tenant_id: Uuid) {
        self.inner.remove(&tenant_id);
    }
}

/// Fallback configuration used when no `kabipay_ops.tenant_database` row
/// exists for a tenant (dev-mode scaffold).
#[derive(Debug, Clone)]
pub struct TenantDbConfig {
    pub db_host: String,
    pub db_port: u16,
    pub db_name: String,
    pub db_user: String,
    pub db_password: String,
    pub schema_name: String,
}

impl TenantDbConfig {
    fn url_for(&self, host: &str, db_name: &str) -> String {
        let base = format!(
            "postgres://{}:{}@{}:{}/{}",
            self.db_user, self.db_password, host, self.db_port, db_name
        );
        apply_postgres_ssl_mode_to_url(&base)
    }
}

/// If `POSTGRES_SSLMODE` is set (e.g. `require` for Aiven / Neon), append `sslmode` to the
/// URL when it is not already present. Managed Postgres typically requires TLS.
pub fn apply_postgres_ssl_mode_to_url(url: &str) -> String {
    let mode = match std::env::var("POSTGRES_SSLMODE") {
        Ok(m) if !m.is_empty() => m,
        _ => return url.to_string(),
    };
    if url.contains("sslmode=") {
        return url.to_string();
    }
    if url.contains('?') {
        format!("{url}&sslmode={mode}")
    } else {
        format!("{url}?sslmode={mode}")
    }
}

fn tenant_search_path_sql(schema_name: &str) -> String {
    let safe = schema_name.replace('"', "\"\"");
    format!(r#"SET search_path = "{safe}","kabipay_ops","public""#)
}

async fn connect_tenant_pool(url: &str, schema_name: &str) -> KabiPayResult<DatabaseConnection> {
    if schema_name.is_empty() {
        return Err(KabiPayError::Internal(
            "tenant schema_name is empty (cannot set search_path)".into(),
        ));
    }

    let sql_arc = Arc::new(tenant_search_path_sql(schema_name));

    let pg_opts = url
        .parse::<sqlx::postgres::PgConnectOptions>()
        .map_err(|e| KabiPayError::Internal(format!("invalid tenant DB URL: {e}")))?
        .disable_statement_logging();

    let pool = PgPoolOptions::new()
        .max_connections(tenant_pool_max_connections())
        .min_connections(0)
        .acquire_timeout(pool_acquire_timeout())
        .idle_timeout(Some(pool_idle_timeout()))
        .test_before_acquire(true)
        .after_connect({
            let sql_arc = Arc::clone(&sql_arc);
            move |conn, _meta| {
                let sql = Arc::clone(&sql_arc);
                Box::pin(async move {
                    sqlx::Executor::execute(conn, sql.as_str()).await?;
                    Ok(())
                })
            }
        })
        .before_acquire({
            let sql_arc = Arc::clone(&sql_arc);
            move |conn, _meta| {
                let sql = Arc::clone(&sql_arc);
                Box::pin(async move {
                    sqlx::Executor::execute(conn, sql.as_str()).await?;
                    Ok(true)
                })
            }
        })
        .connect_with(pg_opts)
        .await
        .map_err(|e| {
            KabiPayError::Internal(format!(
                "failed to open tenant sqlx pool ({schema_name}): {e}"
            ))
        })?;

    Ok(SqlxPostgresConnector::from_sqlx_postgres_pool(pool))
}

/// Resolve (or create) a pooled SeaORM connection for a tenant.
///
/// Strategy:
/// 1. Return the cached handle if present.
/// 2. Look up `kabipay_ops.tenant_database` for an active row matching
///    `tenant_id`. When found, build the connection against that row's
///    `db_host` / `db_name` / `schema_name`.
/// 3. If no active row exists, fall back to the local scaffold
///    (`derive_tenant_schema_name` + `fallback_cfg`) and log a warning.
/// 4. Insert the handle into the cache and return it.
///
/// The returned connection has `search_path` pinned to
/// `<schema>,kabipay_ops,public` so trigger functions like
/// `kabipay_ops.set_updated_at()` still resolve.
pub async fn resolve_tenant_db(
    tenant_id: Uuid,
    ops_db: &DatabaseConnection,
    cache: &TenantDbCache,
    fallback_cfg: &TenantDbConfig,
) -> KabiPayResult<DatabaseConnection> {
    resolve_tenant_handle(tenant_id, ops_db, cache, fallback_cfg)
        .await
        .map(|h| h.conn)
}

/// Like [`resolve_tenant_db`] but exposes the full [`TenantDbHandle`] (handy
/// for resolvers that need the schema name or want to know whether the
/// fallback scaffold was used).
pub async fn resolve_tenant_handle(
    tenant_id: Uuid,
    ops_db: &DatabaseConnection,
    cache: &TenantDbCache,
    fallback_cfg: &TenantDbConfig,
) -> KabiPayResult<TenantDbHandle> {
    if let Some(handle) = cache.inner.get(&tenant_id) {
        return Ok(handle.clone());
    }

    let row = tenant_database::Entity::find()
        .filter(tenant_database::Column::TenantId.eq(tenant_id))
        .filter(tenant_database::Column::IsActive.eq(true))
        .one(ops_db)
        .await?;

    let (db_host, db_name, schema_name, from_ops_row) = match row {
        Some(r) => (r.db_host, r.db_name, r.schema_name, true),
        None => {
            let schema = derive_tenant_schema_name(tenant_id);
            tracing::warn!(
                %tenant_id, schema,
                "no active kabipay_ops.tenant_database row; using derived-schema fallback"
            );
            (
                fallback_cfg.db_host.clone(),
                fallback_cfg.db_name.clone(),
                schema,
                false,
            )
        }
    };

    // When the ops row points at `postgres` (Docker service name) but we're
    // running outside the compose network, fall through to the env-configured
    // host so local `cargo run` still works.
    let effective_host = if is_docker_internal_host(&db_host) && !fallback_cfg.db_host.is_empty() {
        fallback_cfg.db_host.clone()
    } else {
        db_host.clone()
    };

    let url = fallback_cfg.url_for(&effective_host, &db_name);
    let conn = connect_tenant_pool(&url, &schema_name).await.map_err(|e| {
        KabiPayError::Internal(format!(
            "failed to open tenant pool ({schema_name}@{effective_host}/{db_name}): {e}"
        ))
    })?;

    let handle = TenantDbHandle {
        conn,
        schema_name: schema_name.clone(),
        db_host: effective_host,
        db_name,
        from_ops_row,
    };

    cache.inner.insert(tenant_id, handle.clone());
    Ok(handle)
}

fn is_docker_internal_host(host: &str) -> bool {
    // Common docker service / internal names that are unroutable from host.
    matches!(host, "postgres" | "kabipay_postgres")
}

/// Derive the PostgreSQL schema name for a tenant from its UUID.
/// Format: `tenant_<first 8 hex chars>` (no hyphens). Kept as a fallback
/// for dev environments without a populated `kabipay_ops.tenant_database`.
pub fn derive_tenant_schema_name(tenant_id: Uuid) -> String {
    let hex = tenant_id.simple().to_string();
    format!("tenant_{}", &hex[..8])
}

/// One raw session before opening the ORM pool. Surfaces real Postgres errors (TLS, auth,
/// `FATAL: sorry, too many clients`) instead of sqlx's easy-to-misread `PoolTimedOut`.
async fn preflight_ops_session(dsn: &str) -> KabiPayResult<()> {
    let mut conn = sqlx::postgres::PgConnection::connect(dsn).await.map_err(|e| {
        KabiPayError::Internal(format!(
            "postgres session failed (not the connection pool yet): {e}"
        ))
    })?;

    sqlx::query("SET search_path TO kabipay_ops, public")
        .execute(&mut conn)
        .await
        .map_err(|e| {
            KabiPayError::Internal(format!(
                "connected but `SET search_path TO kabipay_ops, public` failed (migrations / schema missing?): {e}"
            ))
        })?;

    conn.close()
        .await
        .map_err(|e| KabiPayError::Internal(format!("closing preflight session: {e}")))
}

fn skip_ops_preflight() -> bool {
    matches!(
        std::env::var("KABIPAY_DB_SKIP_PREFLIGHT").ok().as_deref(),
        Some("1") | Some("true") | Some("yes")
    )
}

/// Build the single ops-plane connection. Called once at service startup.
pub async fn connect_ops_db(dsn: &str) -> KabiPayResult<DatabaseConnection> {
    if !skip_ops_preflight() {
        preflight_ops_session(dsn).await?;
    }

    let mut opts = ConnectOptions::new(dsn.to_string());
    // Default 2 per process so 18-20 local subgraphs stay below a 100-connection DB cap.
    // Raise KABIPAY_DB_POOL_MAX only for consolidated or independently scaled services.
    opts.max_connections(pool_max_connections(2))
        .min_connections(0)
        .connect_timeout(pool_connect_timeout())
        .acquire_timeout(pool_acquire_timeout())
        .idle_timeout(pool_idle_timeout())
        .sqlx_logging(false)
        .set_schema_search_path("kabipay_ops,public");

    Database::connect(opts).await.map_err(|e| {
        KabiPayError::Internal(format!(
            "ORM pool open failed after login succeeded (try KABIPAY_DB_POOL_MAX or fewer running services): {e}"
        ))
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn schema_name_is_deterministic() {
        let id = Uuid::parse_str("abcdef01-0000-0000-0000-000000000000").unwrap();
        assert_eq!(derive_tenant_schema_name(id), "tenant_abcdef01");
    }

    #[test]
    fn tenant_db_config_builds_url() {
        let cfg = TenantDbConfig {
            db_host: "localhost".into(),
            db_port: 5432,
            db_name: "kabipay_dev".into(),
            db_user: "kabipay".into(),
            db_password: "secret".into(),
            schema_name: "tenant_abc12345".into(),
        };
        assert_eq!(
            cfg.url_for("localhost", "kabipay_dev"),
            "postgres://kabipay:secret@localhost:5432/kabipay_dev"
        );
    }

    #[test]
    fn docker_internal_hosts_are_rewritten() {
        assert!(is_docker_internal_host("postgres"));
        assert!(is_docker_internal_host("kabipay_postgres"));
        assert!(!is_docker_internal_host("localhost"));
        assert!(!is_docker_internal_host("db.prod.internal"));
    }

    #[test]
    fn apply_ssl_appends_mode_when_set() {
        std::env::set_var("POSTGRES_SSLMODE", "require");
        assert_eq!(
            apply_postgres_ssl_mode_to_url("postgres://u:p@h:1/db"),
            "postgres://u:p@h:1/db?sslmode=require"
        );
        std::env::remove_var("POSTGRES_SSLMODE");
    }

    #[test]
    fn apply_ssl_skips_if_already_in_url() {
        std::env::set_var("POSTGRES_SSLMODE", "require");
        let u = "postgres://u:p@h:1/db?sslmode=verify-full";
        assert_eq!(apply_postgres_ssl_mode_to_url(u), u);
        std::env::remove_var("POSTGRES_SSLMODE");
    }
}
