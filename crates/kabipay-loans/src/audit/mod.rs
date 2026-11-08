//! Financial integrity checks and immutable results. These APIs never repair balances.
mod events;
mod read;
mod store;
use chrono::{DateTime, NaiveDate, Utc};
pub use events::{acknowledge_financial_events, due_financial_audits};
pub use read::inspect_loan_financials;
use rust_decimal::Decimal;
use serde::{Deserialize, Serialize};
pub use store::{authorize_loan_audit, latest_loan_audit, persist_loan_audit};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct FinancialIssue {
    pub code: String,
    pub reference_id: Option<Uuid>,
}
impl FinancialIssue {
    pub fn new(code: &str, reference_id: Option<Uuid>) -> Self {
        Self {
            code: code.into(),
            reference_id,
        }
    }
}
#[derive(Clone, Debug)]
pub struct RecoverySourceFact {
    pub posting_id: Uuid,
    pub source_kind: String,
    pub source_id: Uuid,
    pub source_revision: i64,
    pub value_date: NaiveDate,
    pub terms_version_id: Uuid,
    pub quote_fingerprint: Option<String>,
    pub principal: Decimal,
    pub interest: Decimal,
}
#[derive(Clone, Debug)]
pub struct LoanFinancialFacts {
    pub loan_id: Uuid,
    pub employee_id: Uuid,
    pub account_version: i64,
    pub checked_at: DateTime<Utc>,
    pub issues: Vec<FinancialIssue>,
    pub recoveries: Vec<RecoverySourceFact>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct LoanFinancialReport {
    pub id: Uuid,
    pub loan_id: Uuid,
    pub employee_id: Uuid,
    pub account_version: i64,
    pub checked_at: DateTime<Utc>,
    pub issues: Vec<FinancialIssue>,
    pub verified_source_count: usize,
    pub fingerprint: String,
}
