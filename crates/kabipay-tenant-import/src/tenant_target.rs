//! Fail-closed mapping and authorization, sharing the application's RBAC projection.
use crate::{contract::ImportPackage, options::ImportOptions};
use anyhow::{bail, Result};
use kabipay_common::{
    context::{PERM_EMPLOYEE_MANAGE, PERM_LEAVE_MANAGE, PERM_PAYROLL_MANAGE},
    db::{resolve_required_tenant_handle, TenantDbCache, TenantDbConfig},
};
use kabipay_db_entities::{
    ops::{tenant, tenant_database},
    tenant::d0005_auth_rbac::user,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement,
};

pub struct VerifiedTenantTarget {
    pub db: DatabaseConnection,
    pub options: ImportOptions,
    pub mapping_id: uuid::Uuid,
    pub connection_host: String,
    pub ops: DatabaseConnection,
}
pub async fn resolve(
    ops: &DatabaseConnection,
    fallback: &TenantDbConfig,
    package: &ImportPackage,
    options: &ImportOptions,
) -> Result<VerifiedTenantTarget> {
    options.validate()?;
    if package.tenant_code != options.tenant_code {
        bail!("TENANT_CODE_DISAGREES");
    }
    let tenants = tenant::Entity::find()
        .filter(tenant::Column::Id.eq(options.tenant_id))
        .filter(tenant::Column::Subdomain.eq(&options.tenant_code))
        .filter(tenant::Column::IsDeleted.eq(false))
        .filter(tenant::Column::Status.eq("ACTIVE"))
        .all(ops)
        .await?;
    if tenants.len() != 1 {
        bail!("TENANT_IDENTITY_UNRESOLVED");
    }
    let mappings = tenant_database::Entity::find()
        .filter(tenant_database::Column::TenantId.eq(options.tenant_id))
        .filter(tenant_database::Column::IsActive.eq(true))
        .all(ops)
        .await?;
    if mappings.len() != 1 {
        bail!("TENANT_MAPPING_AMBIGUOUS");
    }
    let mapping = &mappings[0];
    if mapping.db_type != "POSTGRES"
        || mapping.schema_name != options.schema_name
        || mapping.db_host != options.db_host
        || mapping.db_name != options.db_name
    {
        bail!("TARGET_MAPPING_DISAGREES");
    }
    let handle =
        resolve_required_tenant_handle(options.tenant_id, ops, &TenantDbCache::new(), fallback)
            .await?;
    let expected_host = options
        .connection_host
        .as_deref()
        .unwrap_or(&options.db_host);
    if handle.db_host != expected_host {
        bail!("CONNECTION_HOST_REVIEW_REQUIRED");
    }
    let connection_host = handle.db_host;
    let db = handle.conn;
    let identity = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT current_schema() AS schema,current_database() AS db".to_owned(),
        ))
        .await?
        .ok_or_else(|| anyhow::anyhow!("TARGET_IDENTITY_UNRESOLVED"))?;
    if identity.try_get::<String>("", "schema")? != options.schema_name
        || identity.try_get::<String>("", "db")? != options.db_name
    {
        bail!("CONNECTED_TARGET_DISAGREES");
    }
    let actor = user::Entity::find()
        .filter(user::Column::Id.eq(options.actor_id))
        .filter(user::Column::TenantId.eq(options.tenant_id))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::IsDeleted.eq(false))
        .one(&db)
        .await?;
    if actor.is_none() {
        bail!("IMPORT_ACTOR_INACTIVE");
    }
    let permissions =
        kabipay_auth::rbac::load_client_authorization(&db, options.tenant_id, options.actor_id)
            .await?;
    for code in [PERM_EMPLOYEE_MANAGE, PERM_PAYROLL_MANAGE, PERM_LEAVE_MANAGE] {
        if permissions
            .permission_scopes
            .get(code)
            .is_none_or(|scope| scope != "ALL")
        {
            bail!("IMPORT_ACTOR_UNAUTHORIZED");
        }
    }
    if package
        .employees
        .iter()
        .any(|row| row.tax_settings.is_some() || !row.tax_history.is_empty())
        && permissions
            .permission_scopes
            .get(kabipay_common::context::PERM_TAX_MANAGE)
            .is_none_or(|scope| scope != "ALL")
    {
        bail!("IMPORT_ACTOR_TAX_MANAGE_REQUIRED");
    }
    let migration=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema=$1 AND table_name IN ('tenant_import_run','tenant_import_record','payroll_period_input','payslip_statement','leave_import_history','employee_payroll_rule')",
        [options.schema_name.clone().into()])).await?.ok_or_else(||anyhow::anyhow!("MIGRATION_STATE_UNRESOLVED"))?;
    if migration.try_get::<i64>("", "n")? != 6 {
        bail!("IMPORT_MIGRATIONS_REQUIRED");
    }
    if package.company_payroll_policy.is_some()
        || package
            .employees
            .iter()
            .any(|row| row.tax_settings.is_some() || !row.tax_history.is_empty())
    {
        let state = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM information_schema.tables WHERE table_schema=$1 AND table_name IN ('employee_tax_settings','employee_tax_history','employee_tax_declaration','company_payroll_rule')",
            [options.schema_name.clone().into()])).await?
            .ok_or_else(|| anyhow::anyhow!("MIGRATION_STATE_UNRESOLVED"))?;
        if state.try_get::<i64>("", "n")? != 4 {
            bail!("TAX_IMPORT_MIGRATIONS_REQUIRED");
        }
    }
    let columns=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*) AS n FROM information_schema.columns WHERE table_schema=$1 AND (table_name,column_name) IN (('employee','confirmation_date'),('employee','imported_exit_date'),('employee','imported_last_working_date'),('employee','payroll_excluded'),('employee_bank','account_holder'),('employee_bank','branch_name'),('employee_salary_structure','annual_gross'),('employee_salary_structure','annual_employer_pf'),('salary_component','show_on_payslip'),('leave_import_history','leave_type_id'),('payroll_period_input','revision'))",
        [options.schema_name.clone().into()])).await?.ok_or_else(||anyhow::anyhow!("MIGRATION_STATE_UNRESOLVED"))?;
    if columns.try_get::<i64>("", "n")? != 11 {
        bail!("IMPORT_MIGRATIONS_REQUIRED");
    }
    Ok(VerifiedTenantTarget {
        db,
        options: options.clone(),
        mapping_id: mapping.id,
        connection_host,
        ops: ops.clone(),
    })
}

/// Hold the control-plane mapping stable until the tenant transaction finishes.
/// SHARE locks allow ordinary reads and tenant FK checks while preventing remapping.
pub async fn lock_mapping(target: &VerifiedTenantTarget) -> Result<sea_orm::DatabaseTransaction> {
    use sea_orm::TransactionTrait;
    let transaction = target.ops.begin().await?;
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "SET LOCAL lock_timeout='15s'",
        ))
        .await?;
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "LOCK TABLE kabipay_ops.tenant_database IN SHARE MODE",
        ))
        .await?;
    let options = &target.options;
    let row=transaction.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT t.id FROM kabipay_ops.tenant t WHERE t.id=$1 AND t.subdomain=$2 AND t.status='ACTIVE' AND NOT t.is_deleted FOR SHARE",
        [options.tenant_id.into(),options.tenant_code.clone().into()])).await?;
    if row.is_none() {
        bail!("TARGET_MAPPING_CHANGED_REVIEW_REQUIRED");
    }
    let mappings = tenant_database::Entity::find()
        .filter(tenant_database::Column::TenantId.eq(options.tenant_id))
        .filter(tenant_database::Column::IsActive.eq(true))
        .all(&transaction)
        .await?;
    if mappings.len() != 1
        || mappings[0].id != target.mapping_id
        || mappings[0].db_type != "POSTGRES"
        || mappings[0].schema_name != options.schema_name
        || mappings[0].db_host != options.db_host
        || mappings[0].db_name != options.db_name
    {
        bail!("TARGET_MAPPING_CHANGED_REVIEW_REQUIRED");
    }
    Ok(transaction)
}
