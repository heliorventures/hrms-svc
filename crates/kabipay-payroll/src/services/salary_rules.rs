//! Validate gross-denominated recurring salary independently of employer costs.
use super::payroll_rules::{amount, money};
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmployerPfRule {
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
    let annual_pf = match &input.employer_pf_rule {
        Some(rule) => {
            if rule.rounding != "HALF_UP_2DP" || rule.basis_components.is_empty() {
                return Err(KabiPayError::Validation(
                    "unsupported employer PF rounding or basis".into(),
                ));
            }
            let mut seen = std::collections::HashSet::new();
            let mut base = Decimal::ZERO;
            for code in &rule.basis_components {
                if !seen.insert(code) {
                    return Err(KabiPayError::Validation(
                        "duplicate employer PF basis component".into(),
                    ));
                }
                base += components.get(code).ok_or_else(|| {
                    KabiPayError::Validation("employer PF basis component is missing".into())
                })?;
            }
            if let Some(ceiling) = &rule.ceiling {
                base = base.min(amount(Some(ceiling), "PF ceiling")?);
            }
            let rate = amount(Some(&rule.rate), "employer PF rate")?;
            if rate > Decimal::ONE {
                return Err(KabiPayError::Validation(
                    "employer PF rate exceeds one".into(),
                ));
            }
            Some(money(base * rate) * Decimal::from(12))
        }
        None => None,
    };
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
