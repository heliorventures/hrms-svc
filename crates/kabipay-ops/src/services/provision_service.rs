//! Obsolete schema provisioning is rejected; company databases are provisioned by operator tools.

use kabipay_common::{db::TenantDbCache, KabiPayError, KabiPayResult};
use kabipay_db_entities::ops::tenant;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

pub struct ProvisionOutcome {
    pub tenant: tenant::Model,
    pub schema_name: String,
    pub migrations_ran: bool,
    pub detail: Option<String>,
}

/// Reject the retired metadata-schema path before any database or cache access.
pub async fn provision_tenant(
    _db: &DatabaseConnection,
    _cache: &TenantDbCache,
    _name: String,
    _code: String,
    _country: Option<String>,
    _currency: Option<String>,
    _schema_name_override: Option<String>,
    _run_migrations: bool,
) -> KabiPayResult<ProvisionOutcome> {
    Err(KabiPayError::Validation(
        "Schema-based tenant provisioning through kabipay-ops is disabled; use dedicated company database operator provisioning tools at hrms-database/provisioning/company-databases.".into(),
    ))
}

/// Tenant migrations are managed from the database repo, not from the service process.
pub async fn run_tenant_migrations(
    _db: &DatabaseConnection,
    _cache: &TenantDbCache,
    tenant_id: Uuid,
) -> KabiPayResult<ProvisionOutcome> {
    Err(KabiPayError::Validation(format!(
        "tenant migrations are managed from kabipay-database/scripts; run update-tenant-liquibase.ps1 for tenant {tenant_id}"
    )))
}

#[cfg(test)]
mod tests {
    use kabipay_common::{db::TenantDbCache, KabiPayError};
    use sea_orm::DatabaseConnection;

    #[tokio::test]
    async fn obsolete_provisioning_refuses_before_database_access() {
        for (schema, run_migrations) in [(None, false), (Some("tenant_legacy".into()), true)] {
            let cache = TenantDbCache::new();
            let result = super::provision_tenant(
                &DatabaseConnection::Disconnected,
                &cache,
                "Example company".into(),
                "EXAMPLE".into(),
                Some("IN".into()),
                Some("INR".into()),
                schema,
                run_migrations,
            )
            .await;
            match result {
                Err(KabiPayError::Validation(message)) => {
                    assert!(message.contains("company database operator provisioning"));
                    assert!(message.contains("provisioning/company-databases"));
                }
                Err(error) => panic!("obsolete provisioning must refuse without querying the disconnected DB: {error}"),
                Ok(_) => panic!("obsolete metadata-schema provisioning must not succeed"),
            }
            assert!(cache.is_empty(), "refusal must not invalidate or create tenant pools");
        }
    }
}
