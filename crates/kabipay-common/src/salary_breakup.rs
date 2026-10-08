//! One salary component resolver shared by payroll and annual projections.
use crate::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::{
    employee_salary_component_override, employee_salary_structure, salary_component,
    salary_structure_component,
};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QueryOrder};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
#[derive(Clone, Debug)]
pub struct SalaryBreakupLine {
    pub salary_component_id: Uuid,
    pub component_name: String,
    pub component_code: String,
    pub component_type: String,
    pub calculation_basis: String,
    pub calculation_value: Decimal,
    pub annual_amount: Decimal,
    pub monthly_amount: Decimal,
    pub is_override: bool,
}

#[derive(Clone, Debug)]
pub struct SalaryBreakup {
    pub employee_id: Uuid,
    pub employee_salary_structure_id: Option<Uuid>,
    pub annual_ctc: Decimal,
    pub monthly_gross: Decimal,
    pub monthly_deductions: Decimal,
    pub monthly_net_before_statutory: Decimal,
    pub lines: Vec<SalaryBreakupLine>,
}

pub fn normalize_calculation_basis(calculation_basis: &str) -> KabiPayResult<String> {
    let normalized = calculation_basis.trim().to_ascii_uppercase();
    match normalized.as_str() {
        "FIXED_ANNUAL" | "FIXED_MONTHLY" | "PERCENT_OF_CTC" | "PERCENT_OF_BASIC" => Ok(normalized),
        _ => Err(KabiPayError::Validation(
            "calculation basis must be FIXED_ANNUAL, FIXED_MONTHLY, PERCENT_OF_CTC, or PERCENT_OF_BASIC".into(),
        )),
    }
}

fn amount_from_rule(
    basis: &str,
    value: Decimal,
    annual_ctc: Decimal,
    annual_basic: Decimal,
) -> KabiPayResult<Decimal> {
    match basis {
        "FIXED_ANNUAL" => Ok(value),
        "FIXED_MONTHLY" => Ok(value * Decimal::from(12)),
        "PERCENT_OF_CTC" => Ok((annual_ctc * value / Decimal::from(100)).round_dp(2)),
        "PERCENT_OF_BASIC" => Ok((annual_basic * value / Decimal::from(100)).round_dp(2)),
        _ => Err(KabiPayError::Validation(format!(
            "unsupported calculation basis `{basis}`"
        ))),
    }
}

fn component_rule_value(row: &salary_structure_component::Model) -> Decimal {
    row.calculation_value
        .or(row.percentage_of_basic)
        .or(row.amount)
        .unwrap_or(Decimal::ZERO)
}

fn resolve_basic_annual(
    rules: &[(String, String, Decimal)],
    annual_ctc: Decimal,
    base_code: &str,
    fallback_annual: Decimal,
) -> KabiPayResult<Decimal> {
    let base_code = base_code.trim().to_ascii_uppercase();
    let candidate = rules
        .iter()
        .find(|(code, _, _)| code.eq_ignore_ascii_case(&base_code))
        .or_else(|| {
            rules
                .iter()
                .find(|(code, _, _)| code.eq_ignore_ascii_case("BASIC"))
        });
    if let Some((_, basis, value)) = candidate {
        let basis = normalize_calculation_basis(basis)?;
        if basis == "PERCENT_OF_BASIC" {
            return Err(KabiPayError::Validation(
                "basic salary component cannot be calculated as percentage of basic".into(),
            ));
        }
        return amount_from_rule(&basis, *value, annual_ctc, Decimal::ZERO);
    }
    if fallback_annual > Decimal::ZERO {
        return Ok(fallback_annual);
    }
    Ok(annual_ctc)
}

pub async fn salary_breakup_for_structure<C: ConnectionTrait + Send + Sync>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    employee_structure: employee_salary_structure::Model,
    base_code: &str,
    fallback_monthly_salary: Decimal,
) -> KabiPayResult<SalaryBreakup> {
    let structure_components = salary_structure_component::Entity::find()
        .filter(salary_structure_component::Column::TenantId.eq(tenant_id))
        .filter(
            salary_structure_component::Column::SalaryStructureId
                .eq(employee_structure.salary_structure_id),
        )
        .order_by_asc(salary_structure_component::Column::DisplayOrder)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    if structure_components.is_empty() {
        return Err(KabiPayError::Validation(
            "assigned salary structure has no components".into(),
        ));
    }
    let component_ids = structure_components
        .iter()
        .map(|c| c.salary_component_id)
        .collect::<Vec<_>>();
    let components = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant_id))
        .filter(salary_component::Column::Id.is_in(component_ids))
        .filter(salary_component::Column::IsActive.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let component_map = components
        .into_iter()
        .map(|c| (c.id, c))
        .collect::<HashMap<_, _>>();
    let rows = structure_components
        .into_iter()
        .filter_map(|row| {
            component_map
                .get(&row.salary_component_id)
                .cloned()
                .map(|component| (row, component))
        })
        .collect::<Vec<_>>();
    let fallback_annual = (fallback_monthly_salary * Decimal::from(12)).round_dp(2);
    let overrides = employee_salary_component_override::Entity::find()
        .filter(employee_salary_component_override::Column::TenantId.eq(tenant_id))
        .filter(
            employee_salary_component_override::Column::EmployeeSalaryStructureId
                .eq(employee_structure.id),
        )
        .filter(employee_salary_component_override::Column::IsActive.eq(true))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let override_map = overrides
        .into_iter()
        .map(|o| (o.salary_component_id, o))
        .collect::<HashMap<_, _>>();

    let override_components = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant_id))
        .filter(
            salary_component::Column::Id.is_in(override_map.keys().copied().collect::<Vec<_>>()),
        )
        .filter(salary_component::Column::IsActive.eq(true))
        .all(db)
        .await?;
    let mut rules = rows
        .iter()
        .map(|(row, component)| {
            let (basis, value) = override_map
                .get(&component.id)
                .map(|o| (o.calculation_basis.clone(), o.calculation_value))
                .unwrap_or_else(|| (row.calculation_basis.clone(), component_rule_value(row)));
            (component.code.clone(), basis, value)
        })
        .collect::<Vec<_>>();
    for component in &override_components {
        if !rows.iter().any(|(_, c)| c.id == component.id) {
            let value = &override_map[&component.id];
            rules.push((
                component.code.clone(),
                value.calculation_basis.clone(),
                value.calculation_value,
            ));
        }
    }
    let annual_basic =
        resolve_basic_annual(&rules, employee_structure.ctc, base_code, fallback_annual)?;

    let mut lines = Vec::new();
    let mut seen_components = HashSet::new();
    for (row, component) in rows {
        let override_row = override_map.get(&component.id);
        let basis = override_row
            .map(|o| o.calculation_basis.clone())
            .unwrap_or_else(|| row.calculation_basis.clone());
        let basis = normalize_calculation_basis(&basis)?;
        let value = override_row
            .map(|o| o.calculation_value)
            .unwrap_or_else(|| component_rule_value(&row));
        let annual =
            amount_from_rule(&basis, value, employee_structure.ctc, annual_basic)?.round_dp(2);
        let monthly = (annual / Decimal::from(12)).round_dp(2);
        seen_components.insert(component.id);
        lines.push(SalaryBreakupLine {
            salary_component_id: component.id,
            component_name: component.name,
            component_code: component.code,
            component_type: component.r#type,
            calculation_basis: basis,
            calculation_value: value,
            annual_amount: annual,
            monthly_amount: monthly,
            is_override: override_row.is_some(),
        });
    }
    for (component_id, override_row) in override_map {
        if seen_components.contains(&component_id) {
            continue;
        }
        let Some(component) = salary_component::Entity::find()
            .filter(salary_component::Column::TenantId.eq(tenant_id))
            .filter(salary_component::Column::Id.eq(component_id))
            .filter(salary_component::Column::IsActive.eq(true))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
        else {
            continue;
        };
        let basis = normalize_calculation_basis(&override_row.calculation_basis)?;
        let annual = amount_from_rule(
            &basis,
            override_row.calculation_value,
            employee_structure.ctc,
            annual_basic,
        )?
        .round_dp(2);
        lines.push(SalaryBreakupLine {
            salary_component_id: component.id,
            component_name: component.name,
            component_code: component.code,
            component_type: component.r#type,
            calculation_basis: basis,
            calculation_value: override_row.calculation_value,
            annual_amount: annual,
            monthly_amount: (annual / Decimal::from(12)).round_dp(2),
            is_override: true,
        });
    }
    lines.sort_by(|a, b| a.component_code.cmp(&b.component_code));
    let monthly_gross: Decimal = lines
        .iter()
        .filter(|line| line.component_type.eq_ignore_ascii_case("EARNING"))
        .map(|line| line.monthly_amount)
        .sum();
    let monthly_deductions: Decimal = lines
        .iter()
        .filter(|line| line.component_type.eq_ignore_ascii_case("DEDUCTION"))
        .map(|line| line.monthly_amount)
        .sum();
    Ok(SalaryBreakup {
        employee_id,
        employee_salary_structure_id: Some(employee_structure.id),
        annual_ctc: employee_structure.ctc,
        monthly_gross: monthly_gross.round_dp(2),
        monthly_deductions: monthly_deductions.round_dp(2),
        monthly_net_before_statutory: (monthly_gross - monthly_deductions).round_dp(2),
        lines,
    })
}
