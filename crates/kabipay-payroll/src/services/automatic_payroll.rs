//! Resolve configured deductions, then use the existing period reconciliation engine.
use super::{
    contribution_calculation::calculate_contributions,
    contribution_rules::*,
    payroll_rules::{amount, calculate_period, money, CalculatedPeriod, PeriodInput},
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_tax::domain::{
    calculate_projection, projection::ActualMonth, EvidenceKind, TaxProjection, TaxProjectionInput,
};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WithholdingOverride {
    pub amount: Decimal,
    pub reason: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AutomaticSettings {
    #[serde(default)]
    pub use_employee_configuration: bool,
    #[serde(default)]
    pub lwp_override: Option<super::automatic_period::LwpOverride>,
    pub eligibility: StatutoryEligibility,
    pub withholding_override: Option<WithholdingOverride>,
}
pub struct EmployeePayrollInput {
    pub period: PeriodInput,
    pub regular_components: BTreeMap<String, Decimal>,
    pub month_components: BTreeMap<String, Decimal>,
    pub policy: ContributionPolicy,
    pub projection: TaxProjectionInput,
    pub employer_pf_rule: Option<super::salary_rules::EmployerPfRule>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreparedEmployeePayroll {
    #[serde(default)]
    pub arrears: Vec<super::reviewed_arrears::ReviewedArrear>,
    pub input: PeriodInput,
    pub calculation: CalculatedPeriod,
    pub tax_projection: Option<TaxProjection>,
    pub contribution_evidence: Option<ContributionResult>,
    pub requires_tax_acknowledgement: bool,
    #[serde(default)]
    pub contribution_policy: Option<ContributionPolicy>,
}
pub fn calculate_employee_payroll(
    input: &EmployeePayrollInput,
) -> KabiPayResult<PreparedEmployeePayroll> {
    calculate_with_arrears(input, Decimal::ZERO)
}

pub(crate) fn calculate_with_arrears(
    input: &EmployeePayrollInput,
    arrears: Decimal,
) -> KabiPayResult<PreparedEmployeePayroll> {
    input.policy.validate()?;
    if input.period.historical_lwp_included {
        return Err(KabiPayError::Validation(
            "historical unpaid usage cannot be deducted again".into(),
        ));
    }
    let mut period = input.period.clone();
    let automatic = period
        .automatic
        .clone()
        .ok_or_else(|| KabiPayError::Validation("automatic period settings are required".into()))?;
    let regular: Decimal = input.regular_components.values().copied().sum();
    let month_gross: Decimal = input.month_components.values().copied().sum();
    let days = amount(period.lwp_days.as_deref(), "LWP days")?;
    let lwp = money(regular / Decimal::from(input.policy.lwp_divisor) * days);
    if lwp > month_gross || month_gross <= Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "LWP exceeds earned salary or salary is missing".into(),
        ));
    }
    let earned = month_gross - lwp;
    let mut components = input
        .month_components
        .iter()
        .map(|(code, v)| (code.clone(), money(*v * earned / month_gross)))
        .collect::<BTreeMap<_, _>>();
    let remainder = earned - components.values().copied().sum::<Decimal>();
    if let Some((_, value)) = components.iter_mut().next_back() {
        *value += remainder;
    }
    let overtime = amount(period.variable_allowance_ot.as_deref(), "overtime")?;
    if !overtime.is_zero() {
        *components.entry("OVERTIME".into()).or_default() += overtime;
    }
    let gross: Decimal = components.values().copied().sum();
    if !arrears.is_zero() {
        *components.entry("ARREAR".into()).or_default() += arrears;
    }
    let gross = gross + arrears;
    let incentive = amount(period.incentive.as_deref(), "incentive")?;
    let mut remuneration = components.clone();
    // Always supply the variable code, including zero, for configured formulas.
    *remuneration.entry("INCENTIVE".into()).or_default() += incentive;
    let mut regular_components = input.regular_components.clone();
    for code in ["INCENTIVE", "OVERTIME", "ARREAR"] {
        regular_components.entry(code.into()).or_default();
        remuneration.entry(code.into()).or_default();
    }
    let contributions = calculate_contributions(
        &ContributionInput {
            employer_pf_rule: input.employer_pf_rule.clone(),
            as_of: input.projection.as_of,
            regular_components,
            earned_components: remuneration.clone(),
            eligibility: automatic.eligibility,
        },
        &input.policy,
    )?;
    let mut tax_input = input.projection.clone();
    tax_input
        .actuals
        .retain(|m| m.year != period.year || m.month != period.month as u32);
    tax_input.actuals.push(ActualMonth {
        year: period.year,
        month: period.month as u32,
        components: remuneration,
        tds: None,
        evidence: EvidenceKind::FutureProjection,
    });
    let projection = calculate_projection(&tax_input)?;
    if projection
        .withholding
        .as_ref()
        .is_some_and(|value| value.excess_withholding)
        && automatic.withholding_override.is_none()
    {
        return Err(KabiPayError::Validation("Recorded TDS exceeds projected annual tax. HR must review and enter a reasoned monthly withholding override; payroll will not create a refund.".into()));
    }
    let tds = match automatic.withholding_override {
        Some(value) => {
            kabipay_tax::domain::validate_amount(value.amount)?;
            kabipay_tax::domain::validate_reason(Some(&value.reason))?;
            value.amount
        }
        None => projection.selected_monthly_tds.ok_or_else(|| {
            KabiPayError::Validation(
                "tax inputs need HR review or a reasoned withholding override".into(),
            )
        })?,
    };
    period.fixed_gross = Some(money(regular).to_string());
    period.gross_rule = "SOURCE_OVERRIDE".into();
    period.earned_gross_override = Some(money(gross).to_string());
    period.lwp_divisor = Some(input.policy.lwp_divisor.to_string());
    period.lwp_basis = Some("GROSS".into());
    period.lwp_amount_override = Some(lwp.to_string());
    period.lwp_handling = "SOURCE_GROSS_INCLUDES_REDUCTION".into();
    period.historical_lwp_included = false;
    period.expected_earned_components = components
        .iter()
        .map(|(k, v)| (k.clone(), Some(money(*v).to_string())))
        .collect();
    period.expected_wages.clear();
    period.expected_statement.clear();
    period.statutory_overrides = [
        ("PF", contributions.pf_employee),
        ("ESI", contributions.esi_employee),
        ("PT", contributions.professional_tax),
        ("TDS", tds),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), Some(money(v).to_string())))
    .collect();
    period.expected_employer_contributions = [
        ("pf", contributions.pf_employer),
        ("esi", contributions.esi_employer),
    ]
    .into_iter()
    .map(|(k, v)| (k.into(), Some(money(v).to_string())))
    .collect();
    let calculation = calculate_period(&period)?;
    Ok(PreparedEmployeePayroll {
        arrears: Vec::new(),
        input: period,
        calculation,
        requires_tax_acknowledgement: !projection.history_complete,
        tax_projection: Some(projection),
        contribution_evidence: Some(contributions),
        contribution_policy: Some(input.policy.clone()),
    })
}
