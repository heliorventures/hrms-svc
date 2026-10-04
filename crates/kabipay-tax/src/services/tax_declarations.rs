//! Self-service inputs never write tax_computation or payroll history.
use super::tax_settings::{lock_employee_tax, require_employee};
use crate::domain::{validate_amount, validate_year};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0013_tax_statutory::tax_computation,
    d0093_tax_projection_configuration::employee_tax_declaration as declaration,
};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement};
use uuid::Uuid;

pub fn validate_declaration_calculated_fields(
    taxable: Option<Decimal>,
    final_tax: Option<Decimal>,
    monthly_tds: Option<Decimal>,
) -> KabiPayResult<()> {
    if taxable.is_some() || final_tax.is_some() || monthly_tds.is_some() {
        return Err(KabiPayError::Forbidden(
            "employee declarations cannot supply calculated taxable income, annual tax or TDS"
                .into(),
        ));
    }
    Ok(())
}
pub async fn save_declaration<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    version: Uuid,
    year: i32,
    regime: Option<String>,
    gross: Option<Decimal>,
    deductions: Option<Decimal>,
) -> KabiPayResult<tax_computation::Model> {
    let regime = regime.map(|value| match value.as_str() {
        "OLD_REGIME" => "OLD".into(),
        "NEW_REGIME" => "NEW".into(),
        _ => value,
    });
    validate_year(year)?;
    for amount in [gross, deductions].into_iter().flatten() {
        validate_amount(amount)?;
    }
    if regime
        .as_deref()
        .is_some_and(|v| !matches!(v, "OLD" | "NEW"))
    {
        return Err(KabiPayError::Validation(
            "invalid declared tax regime".into(),
        ));
    }
    let emp = require_employee(db, tenant, employee).await?;
    let actor = emp
        .user_id
        .ok_or_else(|| KabiPayError::Forbidden("declarations require a linked user".into()))?;
    lock_employee_tax(db, tenant, employee).await?;
    let payload = serde_json::json!({"tax_config_version_id":version,"regime":regime,"gross_income":gross,"declared_deductions":deductions});
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_tax_declaration(tenant_id,employee_id,fiscal_year,revision,payload,actor_id) VALUES($1,$2,$3,1,$4,$5) ON CONFLICT(tenant_id,employee_id,fiscal_year) DO UPDATE SET revision=employee_tax_declaration.revision+1,payload=EXCLUDED.payload,actor_id=EXCLUDED.actor_id,updated_at=NOW()",
        [tenant.into(),employee.into(),year.into(),payload.into(),actor.into()])).await?;
    // Old clients receive the existing result unchanged. A first declaration
    // receives an input-only response, never a newly persisted computation.
    Ok(tax_computation::Entity::find()
        .filter(tax_computation::Column::TenantId.eq(tenant))
        .filter(tax_computation::Column::EmployeeId.eq(employee))
        .filter(tax_computation::Column::TaxConfigVersionId.eq(version))
        .filter(tax_computation::Column::FiscalYear.eq(year))
        .one(db)
        .await?
        .unwrap_or(tax_computation::Model {
            id: Uuid::nil(),
            tenant_id: tenant,
            employee_id: employee,
            tax_config_version_id: version,
            fiscal_year: year,
            tax_regime_chosen: regime,
            gross_income: gross,
            total_deductions: deductions,
            taxable_income: None,
            final_tax: None,
            tds_per_month: None,
            computed_at: chrono::Utc::now(),
        }))
}
pub async fn save_approved_total<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
    sum: Decimal,
    actor: Uuid,
) -> KabiPayResult<()> {
    validate_amount(sum)?;
    require_employee(db, tenant, employee).await?;
    lock_employee_tax(db, tenant, employee).await?;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_tax_declaration(tenant_id,employee_id,fiscal_year,revision,payload,approved_deductions,actor_id) VALUES($1,$2,$3,1,'{}',$4,$5) ON CONFLICT(tenant_id,employee_id,fiscal_year) DO UPDATE SET revision=employee_tax_declaration.revision+1,approved_deductions=EXCLUDED.approved_deductions,actor_id=EXCLUDED.actor_id,updated_at=NOW()",
        [tenant.into(),employee.into(),year.into(),sum.into(),actor.into()])).await?;
    Ok(())
}
pub async fn load_declaration<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
) -> KabiPayResult<Option<serde_json::Value>> {
    Ok(declaration::Entity::find().filter(declaration::Column::TenantId.eq(tenant)).filter(declaration::Column::EmployeeId.eq(employee))
        .filter(declaration::Column::FiscalYear.eq(year)).one(db).await?.map(|r| serde_json::json!({"input":r.payload,"approved_deductions":r.approved_deductions,"revision":r.revision})))
}
