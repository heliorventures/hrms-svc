use super::{validate_reason, TaxRegime, WithholdingMethod};
use chrono::{Datelike, NaiveDate};
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use uuid::Uuid;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaxSettingsInput {
    pub regime: TaxRegime,
    pub method: WithholdingMethod,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub percentage: Option<Decimal>,
    #[serde(default)]
    pub basis_components: Vec<String>,
    pub effective_from: NaiveDate,
    pub effective_until: Option<NaiveDate>,
    pub reason: Option<String>,
    #[serde(default)]
    pub resident: Option<bool>,
}
impl TaxSettingsInput {
    pub fn validate(&self) -> KabiPayResult<()> {
        if !(2000..=2199).contains(&self.effective_from.year())
            || self
                .effective_until
                .is_some_and(|end| end < self.effective_from || end.year() > 2199)
        {
            return Err(KabiPayError::Validation(
                "invalid tax settings effective period".into(),
            ));
        }
        if self.method == WithholdingMethod::PercentageOverride {
            validate_reason(self.reason.as_deref())?;
            if self
                .percentage
                .is_none_or(|v| v <= Decimal::ZERO || v > Decimal::ONE || v.scale() > 6)
                || self.basis_components.is_empty()
                || self.basis_components.len() > 64
                || self.basis_components.iter().any(|v| {
                    v.is_empty()
                        || v.len() > 64
                        || !v
                            .bytes()
                            .all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_')
                })
                || self.basis_components.iter().collect::<BTreeSet<_>>().len()
                    != self.basis_components.len()
            {
                return Err(KabiPayError::Validation(
                    "percentage override requires a valid rate and unique component basis".into(),
                ));
            }
        } else if self.percentage.is_some() || !self.basis_components.is_empty() {
            return Err(KabiPayError::Validation(
                "annual projection must not contain percentage override inputs".into(),
            ));
        }
        Ok(())
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TaxSettings {
    pub id: Uuid,
    pub employee_id: Uuid,
    pub revision: i32,
    pub input: TaxSettingsInput,
}
