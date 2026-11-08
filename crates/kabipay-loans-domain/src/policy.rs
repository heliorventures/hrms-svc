use crate::{AllocationOrder, InterestMethod, LoanDomainError, LoanTerms, RecoveryMode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct LoanPolicy {
    pub eligibility: EligibilityPolicy,
    pub exposure: ExposurePolicy,
    pub interest: InterestPolicy,
    pub recovery: RecoveryPolicy,
    pub short_salary: ShortSalaryPolicy,
    pub allocation: AllocationOrder,
    pub excess_credit: ExcessCreditPolicy,
    pub early_settlement: EarlySettlementPolicy,
    pub exit_recovery: ExitRecoveryPolicy,
    pub tax_treatment: TaxTreatmentPolicy,
    pub approval: ApprovalPolicy,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct EligibilityPolicy {
    pub employment_types: Vec<String>,
    pub minimum_service_days: u32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExposurePolicy {
    #[serde(with = "crate::decimal_string")]
    pub per_loan: Decimal,
    #[serde(with = "crate::decimal_string")]
    pub per_employee: Decimal,
    #[serde(with = "crate::decimal_string")]
    pub per_company: Decimal,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct InterestPolicy {
    pub methods: Vec<InterestKind>,
    #[serde(with = "crate::decimal_string")]
    pub maximum_rate: Decimal,
    #[serde(with = "crate::decimal_string")]
    pub maximum_fixed_charge: Decimal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum InterestKind {
    InterestFree,
    OneTimePercentage,
    OneTimeFixed,
    ReducingBalance,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecoveryPolicy {
    pub modes: Vec<RecoveryMode>,
    pub priority: RecoveryPriority,
    #[serde(with = "crate::decimal_string")]
    pub minimum_monthly_amount: Decimal,
    pub max_instalments: u32,
    pub allow_residual: bool,
    pub allow_overrides: bool,
    pub allow_skips: bool,
    pub allow_accrual_pause: bool,
    #[serde(with = "crate::decimal_string")]
    pub minimum_net_pay: Decimal,
    #[serde(with = "crate::decimal_string")]
    pub maximum_net_pay_percentage: Decimal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum RecoveryPriority {
    OldestApprovedFirst,
    NewestApprovedFirst,
    LowestBalanceFirst,
    HighestBalanceFirst,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum ShortSalaryPolicy {
    Block,
    CapAndCarry,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum ExcessCreditPolicy {
    Reject,
    HoldForReview,
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum EarlySettlementPolicy {
    KeepAssessedCharge,
    WaiverRequiresAuthority,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitRecoveryPolicy {
    pub allow_fnf: bool,
    pub allow_continuing: bool,
    pub review_reference: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct TaxTreatmentPolicy {
    pub jurisdiction: String,
    pub review_reference: String,
    pub external_assessment_required: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ApprovalPolicy {
    pub workflow_id: String,
    pub acknowledgement_required: bool,
}
impl LoanPolicy {
    pub fn validate(&self, currency: &crate::Currency) -> Result<(), LoanDomainError> {
        currency.validate()?;
        for value in [
            self.exposure.per_loan,
            self.exposure.per_employee,
            self.exposure.per_company,
            self.interest.maximum_fixed_charge,
            self.recovery.minimum_monthly_amount,
            self.recovery.minimum_net_pay,
        ] {
            currency.validate_amount(value)?;
        }
        if self.exposure.per_loan <= Decimal::ZERO
            || self.exposure.per_employee < self.exposure.per_loan
            || self.exposure.per_company < self.exposure.per_employee
            || self.interest.maximum_rate < Decimal::ZERO
            || self.interest.methods.is_empty()
            || self.recovery.modes.is_empty()
            || self.recovery.minimum_monthly_amount <= Decimal::ZERO
            || self.recovery.max_instalments == 0
            || self.recovery.max_instalments > 1200
            || self.recovery.maximum_net_pay_percentage <= Decimal::ZERO
            || self.recovery.maximum_net_pay_percentage > Decimal::from(100)
            || self.eligibility.employment_types.is_empty()
            || self
                .eligibility
                .employment_types
                .iter()
                .any(|s| s.trim().is_empty())
            || self.approval.workflow_id.trim().is_empty()
            || self.tax_treatment.jurisdiction.trim().is_empty()
            || self.tax_treatment.review_reference.trim().is_empty()
            || (self.exit_recovery.allow_fnf
                && self.exit_recovery.review_reference.trim().is_empty())
        {
            return Err(LoanDomainError::InvalidTerms);
        }
        Ok(())
    }
    pub fn validate_terms(&self, terms: &LoanTerms) -> Result<(), LoanDomainError> {
        self.validate(&terms.currency)?;
        terms.validate()?;
        if terms.approved_principal > self.exposure.per_loan
            || terms.monthly_amount < self.recovery.minimum_monthly_amount
            || terms.max_instalments > self.recovery.max_instalments
            || (terms.allow_residual && !self.recovery.allow_residual)
            || !self.recovery.modes.contains(&terms.recovery)
            || !self
                .interest
                .methods
                .contains(&InterestKind::from(&terms.interest))
            || terms.allocation != self.allocation
        {
            return Err(LoanDomainError::InvalidTerms);
        }
        match &terms.interest {
            InterestMethod::OneTimePercentage { rate }
            | InterestMethod::ReducingBalance {
                annual_rate: rate, ..
            } if *rate > self.interest.maximum_rate => Err(LoanDomainError::InvalidTerms),
            InterestMethod::OneTimeFixed { amount, .. }
                if *amount > self.interest.maximum_fixed_charge =>
            {
                Err(LoanDomainError::InvalidTerms)
            }
            _ => Ok(()),
        }
    }
}
impl From<&InterestMethod> for InterestKind {
    fn from(value: &InterestMethod) -> Self {
        match value {
            InterestMethod::InterestFree => Self::InterestFree,
            InterestMethod::OneTimePercentage { .. } => Self::OneTimePercentage,
            InterestMethod::OneTimeFixed { .. } => Self::OneTimeFixed,
            InterestMethod::ReducingBalance { .. } => Self::ReducingBalance,
        }
    }
}
