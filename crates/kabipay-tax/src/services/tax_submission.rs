//! Stable FY/regime associations for employee declarations and proof evidence.
use super::{tax_declarations, tax_settings};
use crate::domain::{TaxRegime, TaxSettingsInput};
use chrono::{Datelike, NaiveDate};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0013_tax_statutory::tax_configuration_version as config;
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, QueryOrder, Set, Statement};
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize)]
pub struct SubmissionContext {
    pub fiscal_year: i32,
    pub settings: TaxSettingsInput,
    pub declaration: Option<serde_json::Value>,
}
pub fn selection_date(year: i32, today: NaiveDate) -> KabiPayResult<NaiveDate> {
    crate::domain::validate_year(year)?;
    let current_year = today.year() - i32::from(today.month() < 4);
    let month = if year < current_year { 3 } else if year > current_year { 4 } else { today.month() };
    crate::domain::tax_year::month_bounds(year + i32::from(month < 4), month).map(|(start, _)| start)
}
pub fn regime_code(regime: TaxRegime) -> &'static str {
    match regime { TaxRegime::New => "NEW", TaxRegime::Old => "OLD" }
}
pub fn canonical(value: &str) -> Option<&'static str> {
    match value.trim().to_ascii_uppercase().as_str() { "NEW" | "NEW_REGIME" => Some("NEW"), "OLD" | "OLD_REGIME" => Some("OLD"), _ => None }
}
pub async fn context<C: ConnectionTrait>(db: &C, tenant: Uuid, employee: Uuid, year: i32) -> KabiPayResult<Option<SubmissionContext>> {
    tax_settings::require_employee(db, tenant, employee).await?;
    let today = (chrono::Utc::now() + chrono::Duration::minutes(330)).date_naive();
    let date = selection_date(year, today)?;
    let setting = tax_settings::effective_tax_settings(db, tenant, employee, date).await?;
    match setting {
        None => Ok(None),
        Some(setting) => Ok(Some(SubmissionContext { fiscal_year: year, settings: setting.input, declaration: tax_declarations::load_declaration(db, tenant, employee, year).await? })),
    }
}
/// Caller owns the transaction. Serializes automatic definition creation by tenant/FY/regime.
pub async fn ensure_definition<C: ConnectionTrait>(db: &C, tenant: Uuid, year: i32, regime: TaxRegime) -> KabiPayResult<config::Model> {
    crate::domain::validate_year(year)?;
    let regime = regime_code(regime);
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))", [format!("{tenant}:{year}:{regime}:tax-definition").into()])).await?;
    let rows = config::Entity::find().filter(config::Column::TenantId.eq(tenant)).filter(config::Column::FiscalYear.eq(year)).filter(config::Column::CountryCode.eq("IN")).filter(config::Column::IsActive.eq(true)).order_by_asc(config::Column::CreatedAt).order_by_asc(config::Column::Id).all(db).await?;
    if let Some(row) = rows.into_iter().find(|r| r.regime.as_deref().and_then(canonical) == Some(regime)) { return Ok(row); }
    let now = chrono::Utc::now();
    config::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant), fiscal_year: Set(year), regime: Set(Some(regime.into())), country_code: Set("IN".into()), is_active: Set(true), created_at: Set(now), updated_at: Set(now) }.insert(db).await.map_err(Into::into)
}
pub async fn resolve_version<C: ConnectionTrait>(db: &C, tenant: Uuid, employee: Uuid, year: i32, requested: Option<Uuid>, declared_regime: Option<&str>) -> KabiPayResult<config::Model> {
    tax_settings::lock_employee_tax(db, tenant, employee).await?;
    let context = context(db, tenant, employee, year).await?.ok_or_else(|| KabiPayError::Validation("employee tax settings are missing for this financial year".into()))?;
    let regime = regime_code(context.settings.regime);
    if declared_regime.is_some_and(|value| canonical(value) != Some(regime)) {
        return Err(KabiPayError::Validation("declared regime does not match the employee's assigned tax settings; contact HR".into()));
    }
    if let Some(id) = requested {
        let row = config::Entity::find_by_id(id).filter(config::Column::TenantId.eq(tenant)).one(db).await?.ok_or_else(|| KabiPayError::NotFound { entity: "tax_configuration_version", id: id.to_string() })?;
        if row.fiscal_year != year || row.country_code != "IN" || !row.is_active || row.regime.as_deref().and_then(canonical) != Some(regime) {
            return Err(KabiPayError::Validation("tax definition does not match the employee's financial year and assigned regime".into()));
        }
        return Ok(row);
    }
    ensure_definition(db, tenant, year, context.settings.regime).await
}
