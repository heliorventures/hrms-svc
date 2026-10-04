//! Preparation and finalization share the same persistence contract.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::salary_component;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

pub async fn validate<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    calculation: &super::payroll_rules::CalculatedPeriod,
) -> KabiPayResult<()> {
    if calculation.components.is_empty() {
        return Err(KabiPayError::Validation(
            "Earned salary components are required".into(),
        ));
    }
    let mut codes = calculation.components.keys().cloned().collect::<Vec<_>>();
    if !calculation.incentive.is_zero() {
        codes.push("INCENTIVE".into());
    }
    codes.sort();
    codes.dedup();
    let catalog = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant))
        .filter(salary_component::Column::Code.is_in(codes.clone()))
        .all(db)
        .await?;
    for code in codes {
        let matches = catalog
            .iter()
            .filter(|c| c.code == code)
            .collect::<Vec<_>>();
        if matches.len() != 1 || !matches[0].is_active || matches[0].r#type != "EARNING" {
            return Err(KabiPayError::Validation(format!("Configure {code} as one active EARNING component, including its tax treatment, before calculating payroll")));
        }
    }
    Ok(())
}
