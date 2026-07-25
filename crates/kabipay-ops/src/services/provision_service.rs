//! Tenant metadata provisioning (ops plane). Database migrations and seed data are owned by kabipay-database scripts.

use chrono::Utc;
use kabipay_common::{
    db::{derive_tenant_schema_name, TenantDbCache},
    deterministic_tenant_database_row_uuid, deterministic_tenant_uuid, KabiPayError, KabiPayResult,
};
use kabipay_db_entities::ops::{tenant, tenant_database};
use sea_orm::{ConnectionTrait, DatabaseConnection, EntityTrait, Set, TransactionTrait};
use uuid::Uuid;

fn validate_tenant_code(code: &str) -> KabiPayResult<()> {
    let c = code.trim();
    if c.len() < 2 || c.len() > 32 {
        return Err(KabiPayError::Validation(
            "tenant code must be 2–32 characters".into(),
        ));
    }
    let mut chars = c.chars();
    let Some(first) = chars.next() else {
        return Err(KabiPayError::Validation("tenant code is empty".into()));
    };
    if !first.is_ascii_alphanumeric() {
        return Err(KabiPayError::Validation(
            "tenant code must start with a letter or digit".into(),
        ));
    }
    for ch in std::iter::once(first).chain(chars) {
        if ch.is_ascii_alphanumeric() || ch == '_' || ch == '-' {
            continue;
        }
        return Err(KabiPayError::Validation(
            "tenant code may only contain a-z, 0-9, underscore, hyphen".into(),
        ));
    }
    Ok(())
}

fn validate_schema_override(schema: &str) -> KabiPayResult<()> {
    if schema.len() < 8 || schema.len() > 50 {
        return Err(KabiPayError::Validation(
            "schema name length invalid".into(),
        ));
    }
    if !schema.starts_with("tenant_") {
        return Err(KabiPayError::Validation(
            "schema name must start with tenant_".into(),
        ));
    }
    for ch in schema.chars().skip(7) {
        if ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '_' {
            continue;
        }
        return Err(KabiPayError::Validation(
            "schema name may only contain a-z, 0-9, underscore after tenant_".into(),
        ));
    }
    Ok(())
}

pub struct ProvisionOutcome {
    pub tenant: tenant::Model,
    pub schema_name: String,
    pub migrations_ran: bool,
    pub detail: Option<String>,
}

/// Create/update ops rows, create PostgreSQL schema, optionally run Liquibase tenant changelog.
pub async fn provision_tenant(
    db: &DatabaseConnection,
    cache: &TenantDbCache,
    name: String,
    code: String,
    country: Option<String>,
    currency: Option<String>,
    schema_name_override: Option<String>,
    run_migrations: bool,
) -> KabiPayResult<ProvisionOutcome> {
    validate_tenant_code(&code)?;
    let code = code.trim().to_string();

    let tenant_id = deterministic_tenant_uuid(&code);
    let tenant_db_row_id = deterministic_tenant_database_row_uuid(&code);

    let schema_name = if let Some(ref s) = schema_name_override {
        validate_schema_override(s)?;
        s.clone()
    } else {
        derive_tenant_schema_name(tenant_id)
    };

    if let Some(existing) = tenant::Entity::find_by_id(tenant_id).one(db).await? {
        if existing.status == "TERMINATED" {
            return Err(KabiPayError::Conflict(
                "tenant is terminated; restore it before re-provisioning".into(),
            ));
        }
    }

    let country = country.unwrap_or_else(|| "IN".into());
    let currency = currency.unwrap_or_else(|| "INR".into());
    let subdomain = code.to_lowercase();

    let txn = db.begin().await?;

    let sql = format!(
        "CREATE SCHEMA IF NOT EXISTS {} AUTHORIZATION CURRENT_USER",
        schema_name
    );
    txn.execute_unprepared(&sql).await.map_err(|e| {
        KabiPayError::Internal(format!("create schema {schema_name}: {e}"))
    })?;

    let now = Utc::now();
    let tenant_am = tenant::ActiveModel {
        id: Set(tenant_id),
        name: Set(name.clone()),
        status: Set("PROVISIONING".into()),
        plan: Set(None),
        country: Set(Some(country.clone())),
        timezone: Set(None),
        currency: Set(Some(currency.clone())),
        gstin: Set(None),
        pan: Set(None),
        registered_address: Set(None),
        logo_url: Set(None),
        primary_color: Set(None),
        subdomain: Set(Some(subdomain.clone())),
        account_manager_id: Set(None),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };

    use sea_orm::sea_query::OnConflict;
    tenant::Entity::insert(tenant_am)
        .on_conflict(
            OnConflict::column(tenant::Column::Id)
                .update_columns([
                    tenant::Column::Name,
                    tenant::Column::Country,
                    tenant::Column::Currency,
                    tenant::Column::Subdomain,
                    tenant::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(&txn)
        .await?;

    let db_host = std::env::var("POSTGRES_HOST").unwrap_or_else(|_| "localhost".into());
    let db_name = std::env::var("POSTGRES_DB").unwrap_or_else(|_| "kabipay_dev".into());

    let tdb_am = tenant_database::ActiveModel {
        id: Set(tenant_db_row_id),
        tenant_id: Set(tenant_id),
        db_type: Set("POSTGRES".into()),
        db_host: Set(db_host),
        db_name: Set(db_name),
        schema_name: Set(schema_name.clone()),
        is_active: Set(true),
        created_at: Set(now),
        updated_at: Set(now),
    };

    tenant_database::Entity::insert(tdb_am)
        .on_conflict(
            OnConflict::column(tenant_database::Column::Id)
                .update_columns([
                    tenant_database::Column::TenantId,
                    tenant_database::Column::DbHost,
                    tenant_database::Column::DbName,
                    tenant_database::Column::SchemaName,
                    tenant_database::Column::IsActive,
                    tenant_database::Column::UpdatedAt,
                ])
                .to_owned(),
        )
        .exec(&txn)
        .await?;

    txn.commit().await?;

    cache.invalidate(tenant_id);

    let detail = Some(if run_migrations {
        "runMigrations is ignored by kabipay-svc; run tenant provisioning or migrations from kabipay-database/scripts".into()
    } else {
        "tenant remains PROVISIONING until database provisioning/migrations run from kabipay-database/scripts".into()
    });

    let tenant_row = tenant::Entity::find_by_id(tenant_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("tenant row missing after provision".into()))?;

    Ok(ProvisionOutcome {
        tenant: tenant_row,
        schema_name,
        migrations_ran: false,
        detail,
    })
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
