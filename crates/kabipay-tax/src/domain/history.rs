use super::{validate_amount, validate_reason, validate_year, CoverageStatus, EvidenceKind};
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TaxHistoryEntry {
    pub fiscal_year: i32,
    pub period_start: NaiveDate,
    pub period_end: NaiveDate,
    /// CURRENT or a stable identifier for the previous employer.
    pub employer: String,
    pub source_key: String,
    #[serde(with = "rust_decimal::serde::str")]
    pub earnings: Decimal,
    pub components: BTreeMap<String, Decimal>,
    #[serde(default, with = "rust_decimal::serde::str_option")]
    pub tds: Option<Decimal>,
    pub coverage: CoverageStatus,
    pub reason: String,
    pub evidence: EvidenceKind,
}
impl TaxHistoryEntry {
    pub fn validate(&self) -> KabiPayResult<()> {
        validate_year(self.fiscal_year)?;
        validate_reason(Some(&self.reason))?;
        validate_amount(self.earnings)?;
        if let Some(tds) = self.tds {
            validate_amount(tds)?;
        }
        let start = NaiveDate::from_ymd_opt(self.fiscal_year, 4, 1);
        let end = NaiveDate::from_ymd_opt(self.fiscal_year + 1, 4, 1);
        if start.is_none_or(|v| self.period_start < v)
            || end.is_none_or(|v| self.period_end >= v)
            || self.period_start > self.period_end
            || self.source_key.trim().is_empty()
            || self.source_key.len() > 128
            || self.employer.trim().is_empty()
            || self.employer.len() > 128
            || self.evidence != EvidenceKind::ImportedActual
            || (self.coverage == CoverageStatus::Complete && self.tds.is_none())
        {
            return Err(KabiPayError::Validation(
                "invalid actual-history source, coverage or fiscal period".into(),
            ));
        }
        if self.components.len() > 64 {
            return Err(KabiPayError::Validation(
                "too many history components".into(),
            ));
        }
        for (code, amount) in &self.components {
            if code.is_empty() || code.len() > 64 {
                return Err(KabiPayError::Validation("invalid component code".into()));
            }
            validate_amount(*amount)?;
        }
        if self.components.values().copied().sum::<Decimal>() != self.earnings {
            return Err(KabiPayError::Validation(
                "history components must reconcile to earnings".into(),
            ));
        }
        Ok(())
    }
}
