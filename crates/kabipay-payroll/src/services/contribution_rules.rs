use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Copy, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum WageClassification {
    Included,
    ExcludedWithAddback,
    ExcludedOutsideAddback,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ComponentFormula {
    pub weights: BTreeMap<String, Decimal>,
    pub rate: Decimal,
    pub ceiling: Option<Decimal>,
    pub rounding: String,
}
impl ComponentFormula {
    pub fn validate(&self) -> KabiPayResult<()> {
        if let Some(ceiling) = self.ceiling {
            kabipay_tax::domain::validate_amount(ceiling)?;
        }
        if self.weights.is_empty()
            || self.weights.len() > 64
            || self.rate < Decimal::ZERO
            || self.rate > Decimal::ONE
            || self.weights.iter().any(|(code, v)| {
                code.is_empty() || code.len() > 64 || *v < Decimal::ZERO || *v > Decimal::ONE
            })
            || self.ceiling.is_some_and(|v| v < Decimal::ZERO)
            || !matches!(
                self.rounding.as_str(),
                "HALF_UP_2DP" | "CEIL_RUPEE" | "HALF_UP_RUPEE"
            )
        {
            return Err(KabiPayError::Validation(
                "invalid structured contribution formula".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContributionPolicy {
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub lwp_divisor: u32,
    pub origin: String,
    pub reason: String,
    pub pf_employee: ComponentFormula,
    pub pf_employer: ComponentFormula,
    pub esi_basis: ComponentFormula,
    pub esi_employer_rate: Decimal,
    pub company_esi_covered: bool,
    pub esi_mode: String,
    pub classifications: BTreeMap<String, WageClassification>,
    pub professional_tax: Option<Decimal>,
}
impl ContributionPolicy {
    pub fn validate(&self) -> KabiPayResult<()> {
        kabipay_tax::domain::validate_reason(Some(&self.reason))?;
        if let Some(pt) = self.professional_tax {
            kabipay_tax::domain::validate_amount(pt)?;
        }
        for formula in [&self.pf_employee, &self.pf_employer] {
            formula.validate()?;
        }
        if self.esi_mode == "CUSTOM_COMPONENTS" && self.company_esi_covered {
            self.esi_basis.validate()?;
        }
        if !(1..=31).contains(&self.lwp_divisor)
            || self
                .effective_until
                .is_some_and(|v| v < self.effective_from)
            || !matches!(
                self.origin.as_str(),
                "IMPORTED_CLIENT_RULE" | "HR_CONFIGURATION" | "STATUTORY_VERSION"
            )
            || !matches!(
                self.esi_mode.as_str(),
                "CUSTOM_COMPONENTS" | "INDIA_COSS_2025"
            )
            || self.esi_employer_rate < Decimal::ZERO
            || self.esi_employer_rate > Decimal::ONE
        {
            return Err(KabiPayError::Validation(
                "invalid effective contribution policy".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EsiInput {
    pub as_of: NaiveDate,
    pub company_covered: bool,
    pub employee_eligible: Option<bool>,
    pub regular_wages: Decimal,
    pub earned_wages: Decimal,
    pub continuation_until: Option<NaiveDate>,
    pub disability: Option<bool>,
    pub average_daily_wage: Option<Decimal>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct EsiResult {
    pub employee: Decimal,
    pub employer: Decimal,
    pub wage_basis: Decimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StatutoryEligibility {
    #[serde(default)]
    pub professional_tax: Option<Decimal>,
    pub pf_applicable: Option<bool>,
    pub esi_applicable: Option<bool>,
    pub esi_continuation_until: Option<NaiveDate>,
    pub disability: Option<bool>,
    pub average_daily_wage: Option<Decimal>,
}
#[derive(Clone, Debug)]
pub struct ContributionInput {
    pub as_of: NaiveDate,
    pub regular_components: BTreeMap<String, Decimal>,
    pub earned_components: BTreeMap<String, Decimal>,
    pub eligibility: StatutoryEligibility,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ContributionResult {
    pub pf_employee: Decimal,
    pub pf_employer: Decimal,
    pub esi_employee: Decimal,
    pub esi_employer: Decimal,
    pub professional_tax: Decimal,
    pub esi_wage_basis: Decimal,
    pub origin: String,
    pub reason: String,
}
