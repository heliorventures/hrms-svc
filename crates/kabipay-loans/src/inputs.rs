use chrono::NaiveDate;
use kabipay_loans_domain::{Currency, LoanPolicy, LoanTerms, RecoveryMode};
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

/// Closed command vocabulary. The tenant, actor and authority never come from input.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(
    tag = "command",
    rename_all = "camelCase",
    rename_all_fields = "camelCase",
    deny_unknown_fields
)]
pub enum LoanCommand {
    PublishPolicy {
        key: String,
        currency: Currency,
        effective_from: NaiveDate,
        effective_to: Option<NaiveDate>,
        rules: LoanPolicy,
    },
    RetirePolicy {
        policy_id: Uuid,
        reason: String,
    },
    SaveRequest {
        request_id: Option<Uuid>,
        employee_id: Option<Uuid>,
        policy_id: Uuid,
        #[serde(with = "kabipay_loans_domain::decimal_string")]
        amount: Decimal,
        purpose: String,
        notes: Option<String>,
        preferences: serde_json::Value,
    },
    SubmitRequest {
        request_id: Option<Uuid>,
        employee_id: Option<Uuid>,
        policy_id: Uuid,
        #[serde(with = "kabipay_loans_domain::decimal_string")]
        amount: Decimal,
        purpose: String,
        notes: Option<String>,
        preferences: serde_json::Value,
    },
    DecideRequest {
        request_id: Uuid,
        step_id: Uuid,
        decision: RequestDecision,
        reason: String,
        terms: Option<LoanTerms>,
        effective_from: Option<NaiveDate>,
        first_due_date: Option<NaiveDate>,
        agreement_reference: Option<String>,
    },
    WithdrawRequest {
        request_id: Uuid,
        reason: String,
    },
    RecordDisbursement {
        loan_id: Uuid,
        payment: RecordedPayment,
    },
    RecordReceipt {
        loan_id: Uuid,
        payment: RecordedPayment,
    },
    SetDeduction {
        loan_id: Uuid,
        effective_from: NaiveDate,
        first_due_date: NaiveDate,
        #[serde(with = "kabipay_loans_domain::decimal_string")]
        amount: Decimal,
        recovery: RecoveryMode,
        reason: String,
    },
    SetPeriodOverride {
        loan_id: Uuid,
        period_start: NaiveDate,
        #[serde(with = "kabipay_loans_domain::decimal_string::optional")]
        amount: Option<Decimal>,
        pause_interest: bool,
        reason: String,
    },
    ReversePosting {
        loan_id: Uuid,
        posting_id: Uuid,
        reason: String,
        reconciliation_reference: String,
        review_fingerprint: Option<String>,
    },
}
#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub enum RequestDecision {
    Approve,
    Return,
    Reject,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct RecordedPayment {
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub amount: Decimal,
    pub value_date: NaiveDate,
    pub method: String,
    pub external_reference: String,
    /// An authorized document reference or reviewed external evidence, never a public URL.
    pub evidence_reference: String,
}
impl LoanCommand {
    pub fn permission(&self) -> crate::LoanPermission {
        use crate::LoanPermission::*;
        match self {
            Self::PublishPolicy { .. } | Self::RetirePolicy { .. } => Policy,
            Self::SaveRequest { .. }
            | Self::SubmitRequest { .. }
            | Self::WithdrawRequest { .. } => Submit,
            Self::DecideRequest { .. } => Approve,
            Self::RecordDisbursement { .. } => Disburse,
            Self::RecordReceipt { .. } => Repay,
            Self::SetDeduction { .. } | Self::SetPeriodOverride { .. } => Manage,
            Self::ReversePosting { .. } => Correct,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanCommandResult {
    pub record_id: Uuid,
    pub loan_id: Option<Uuid>,
    pub employee_id: Option<Uuid>,
    pub version: i64,
    pub state: String,
    pub posting_ids: Vec<Uuid>,
    #[serde(with = "kabipay_loans_domain::decimal_string")]
    pub unapplied_credit: Decimal,
}
