//! Validate gross-denominated recurring salary independently of employer costs.
use super::payroll_rules::{amount, money};
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmployerPfRule {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fixed_monthly_amount: Option<String>,
    pub basis_components: Vec<String>,
    pub rate: String,
    pub ceiling: Option<String>,
    pub rounding: String,
    pub origin: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecurringSalary {
    pub monthly_gross: String,
    pub annual_gross: String,
    pub components: BTreeMap<String, String>,
    pub component_ratios: BTreeMap<String, String>,
    pub calculation_basis: String,
    pub employer_pf_rule: Option<EmployerPfRule>,
    pub annual_employer_pf: Option<String>,
    pub annual_ctc: Option<String>,
}
pub struct ValidatedSalary {
    pub annual_gross: Decimal,
    pub annual_employer_pf: Option<Decimal>,
    pub annual_ctc: Option<Decimal>,
    pub components: BTreeMap<String, Decimal>,
}

pub fn validate_recurring_salary(input: &RecurringSalary) -> KabiPayResult<ValidatedSalary> {
    let monthly = money(amount(Some(&input.monthly_gross), "monthly gross")?);
    let annual = money(amount(Some(&input.annual_gross), "annual gross")?);
    if monthly.is_zero()
        || monthly * Decimal::from(12) != annual
        || input.calculation_basis != "PERCENT_OF_GROSS"
    {
        return Err(KabiPayError::Validation(
            "recurring gross or calculation basis is inconsistent".into(),
        ));
    }
    let mut components = BTreeMap::new();
    let mut ratios = Decimal::ZERO;
    for (code, value) in &input.components {
        if code.trim().is_empty() || code.len() > 64 {
            return Err(KabiPayError::Validation("invalid component code".into()));
        }
        let ratio = amount(
            input.component_ratios.get(code).map(String::as_str),
            "component ratio",
        )?;
        let value = money(amount(Some(value), "recurring component")?);
        if (value - money(monthly * ratio)).abs() > Decimal::new(2, 2) {
            return Err(KabiPayError::Validation(
                "recurring component does not match its gross ratio".into(),
            ));
        }
        ratios += ratio;
        components.insert(code.clone(), value);
    }
    if components.is_empty()
        || (ratios - Decimal::ONE).abs() > Decimal::new(1, 6)
        || components.values().copied().sum::<Decimal>() != monthly
    {
        return Err(KabiPayError::Validation(
            "recurring components do not reconcile to gross".into(),
        ));
    }
    let annual_pf = input.employer_pf_rule.as_ref().map(|rule| super::employer_pf::monthly(rule, &components).map(|value| value * Decimal::from(12))).transpose()?;
    let supplied_pf = input
        .annual_employer_pf
        .as_deref()
        .map(|v| amount(Some(v), "annual employer PF").map(money))
        .transpose()?;
    let ctc = annual_pf.map(|pf| annual + pf);
    let supplied_ctc = input
        .annual_ctc
        .as_deref()
        .map(|v| amount(Some(v), "annual CTC").map(money))
        .transpose()?;
    if annual_pf != supplied_pf || ctc != supplied_ctc {
        return Err(KabiPayError::Validation(
            "annual employer cost or CTC does not reconcile".into(),
        ));
    }
    Ok(ValidatedSalary {
        annual_gross: annual,
        annual_employer_pf: annual_pf,
        annual_ctc: ctc,
        components,
    })
}
