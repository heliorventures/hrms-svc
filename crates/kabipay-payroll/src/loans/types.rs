use async_graphql::{InputObject, Json, SimpleObject};
use kabipay_loans::{
    LoanAccountView, LoanCommandResult, LoanPage, LoanPaymentView, LoanPolicyView, LoanPostingView,
    LoanRequestView, LoanScheduleView,
};
use serde_json::Value;

#[derive(SimpleObject)]
#[graphql(name = "LoanAccount")]
pub struct LoanAccountDto {
    pub id: String,
    pub employee_id: String,
    pub request_id: String,
    pub loan_number: String,
    pub approved_principal: String,
    pub currency: String,
    pub minor_units: i32,
    pub state: String,
    pub funding_state: String,
    pub version: i64,
    pub principal: String,
    pub interest: String,
    pub terms: Json<Value>,
}
impl From<LoanAccountView> for LoanAccountDto {
    fn from(v: LoanAccountView) -> Self {
        Self {
            id: v.id.to_string(),
            employee_id: v.employee_id.to_string(),
            request_id: v.request_id.to_string(),
            loan_number: v.loan_number,
            approved_principal: v.approved_principal,
            currency: v.currency,
            minor_units: v.minor_units,
            state: v.state,
            funding_state: v.funding_state,
            version: v.version,
            principal: v.principal,
            interest: v.interest,
            terms: Json(v.terms),
        }
    }
}
#[derive(SimpleObject)]
#[graphql(name = "LoanRequest")]
pub struct LoanRequestDto {
    pub id: String,
    pub employee_id: String,
    pub requested_amount: String,
    pub currency: String,
    pub purpose: String,
    pub employee_notes: Option<String>,
    pub state: String,
    pub version: i64,
    pub workflow_instance_id: Option<String>,
    pub current_step_id: Option<String>,
}
impl From<LoanRequestView> for LoanRequestDto {
    fn from(v: LoanRequestView) -> Self {
        Self {
            id: v.id.to_string(),
            employee_id: v.employee_id.to_string(),
            requested_amount: v.requested_amount,
            currency: v.currency,
            purpose: v.purpose,
            employee_notes: v.employee_notes,
            state: v.state,
            version: v.version,
            workflow_instance_id: v.workflow_instance_id.map(|id| id.to_string()),
            current_step_id: v.current_step_id.map(|id| id.to_string()),
        }
    }
}
#[derive(SimpleObject)]
#[graphql(name = "LoanPolicy")]
pub struct LoanPolicyDto {
    pub id: String,
    pub key: String,
    pub version: i32,
    pub status: String,
    pub currency: String,
    pub minor_units: i32,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub rules: Json<Value>,
}
impl From<LoanPolicyView> for LoanPolicyDto {
    fn from(v: LoanPolicyView) -> Self {
        Self {
            id: v.id.to_string(),
            key: v.key,
            version: v.version,
            status: v.status,
            currency: v.currency,
            minor_units: v.minor_units,
            effective_from: v.effective_from.to_string(),
            effective_to: v.effective_to.map(|date| date.to_string()),
            rules: Json(v.rules),
        }
    }
}
#[derive(SimpleObject)]
#[graphql(name = "LoanPosting")]
pub struct LoanPostingDto {
    pub id: String,
    pub kind: String,
    pub source_kind: String,
    pub source_id: String,
    pub value_date: String,
    pub amount: String,
    pub principal_delta: String,
    pub interest_delta: String,
    pub reversal_of: Option<String>,
}
impl From<LoanPostingView> for LoanPostingDto {
    fn from(v: LoanPostingView) -> Self {
        Self {
            id: v.id.to_string(),
            kind: v.kind,
            source_kind: v.source_kind,
            source_id: v.source_id.to_string(),
            value_date: v.value_date.to_string(),
            amount: v.amount,
            principal_delta: v.principal_delta,
            interest_delta: v.interest_delta,
            reversal_of: v.reversal_of.map(|id| id.to_string()),
        }
    }
}
#[derive(SimpleObject)]
#[graphql(name = "LoanCommandResult")]
pub struct LoanResultDto {
    pub record_id: String,
    pub loan_id: Option<String>,
    pub employee_id: Option<String>,
    pub version: i64,
    pub state: String,
    pub posting_ids: Vec<String>,
    pub unapplied_credit: String,
}
impl From<LoanCommandResult> for LoanResultDto {
    fn from(v: LoanCommandResult) -> Self {
        Self {
            record_id: v.record_id.to_string(),
            loan_id: v.loan_id.map(|id| id.to_string()),
            employee_id: v.employee_id.map(|id| id.to_string()),
            version: v.version,
            state: v.state,
            posting_ids: v.posting_ids.into_iter().map(|id| id.to_string()).collect(),
            unapplied_credit: v.unapplied_credit.to_string(),
        }
    }
}
#[derive(SimpleObject)]
#[graphql(name = "LoanPageInfo")]
pub struct LoanPageInfo {
    pub end_cursor: Option<String>,
    pub has_next_page: bool,
}
macro_rules! connection {
    ($name:ident,$node:ty,$view:ty) => {
        #[derive(SimpleObject)]
        pub struct $name {
            pub nodes: Vec<$node>,
            pub page_info: LoanPageInfo,
        }
        impl From<LoanPage<$view>> for $name {
            fn from(page: LoanPage<$view>) -> Self {
                Self {
                    nodes: page.nodes.into_iter().map(Into::into).collect(),
                    page_info: LoanPageInfo {
                        end_cursor: page.end_cursor.map(|id| id.to_string()),
                        has_next_page: page.has_next_page,
                    },
                }
            }
        }
    };
}
connection!(LoanAccountConnection, LoanAccountDto, LoanAccountView);
connection!(LoanRequestConnection, LoanRequestDto, LoanRequestView);
connection!(LoanPolicyConnection, LoanPolicyDto, LoanPolicyView);
connection!(LoanPostingConnection, LoanPostingDto, LoanPostingView);
#[derive(SimpleObject)]
#[graphql(name = "LoanPayment")]
pub struct LoanPaymentDto {
    pub id: String,
    pub kind: String,
    pub amount: String,
    pub value_date: String,
    pub method: String,
    pub external_reference: String,
    pub unapplied_credit: String,
}
impl From<LoanPaymentView> for LoanPaymentDto {
    fn from(v: LoanPaymentView) -> Self {
        Self {
            id: v.id.to_string(),
            kind: v.kind,
            amount: v.amount,
            value_date: v.value_date.to_string(),
            method: v.method,
            external_reference: v.external_reference,
            unapplied_credit: v.unapplied_credit,
        }
    }
}
#[derive(SimpleObject)]
#[graphql(name = "LoanSchedule")]
pub struct LoanScheduleDto {
    pub id: String,
    pub version: i64,
    pub effective_from: String,
    pub monthly_amount: String,
    pub recovery_mode: String,
    pub is_projection: bool,
}
impl From<LoanScheduleView> for LoanScheduleDto {
    fn from(v: LoanScheduleView) -> Self {
        Self {
            id: v.id.to_string(),
            version: v.version,
            effective_from: v.effective_from.to_string(),
            monthly_amount: v.monthly_amount,
            recovery_mode: v.recovery_mode,
            is_projection: v.is_projection,
        }
    }
}
connection!(LoanPaymentConnection, LoanPaymentDto, LoanPaymentView);
connection!(LoanScheduleConnection, LoanScheduleDto, LoanScheduleView);

#[derive(InputObject)]
pub struct LoanCommandMetaInput {
    pub idempotency_key: String,
    pub expected_version: i64,
}
#[derive(InputObject)]
pub struct SubmitLoanRequestInput {
    pub request_id: Option<String>,
    pub employee_id: Option<String>,
    pub policy_id: String,
    pub amount: String,
    pub purpose: String,
    pub notes: Option<String>,
    pub preferences: Json<Value>,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct PublishLoanPolicyInput {
    pub key: String,
    pub currency: String,
    pub minor_units: u32,
    pub effective_from: String,
    pub effective_to: Option<String>,
    pub rules: Json<Value>,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct DecideLoanRequestInput {
    pub request_id: String,
    pub step_id: String,
    pub decision: LoanRequestDecision,
    pub reason: String,
    pub terms: Option<Json<Value>>,
    pub effective_from: Option<String>,
    pub first_due_date: Option<String>,
    pub agreement_reference: Option<String>,
    pub meta: LoanCommandMetaInput,
}
#[derive(async_graphql::Enum, Clone, Copy, Eq, PartialEq)]
pub enum LoanRequestDecision {
    Approve,
    Return,
    Reject,
}
#[derive(async_graphql::Enum, Clone, Copy, Eq, PartialEq)]
pub enum LoanRecoveryMode {
    Payroll,
    External,
    Mixed,
}
#[derive(InputObject)]
pub struct WithdrawLoanRequestInput {
    pub request_id: String,
    pub reason: String,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct RecordLoanPaymentInput {
    pub loan_id: String,
    pub amount: String,
    pub value_date: String,
    pub method: String,
    pub external_reference: String,
    pub evidence_reference: String,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct SetLoanDeductionInput {
    pub loan_id: String,
    pub effective_from: String,
    pub first_due_date: String,
    pub amount: String,
    pub recovery: LoanRecoveryMode,
    pub reason: String,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct SetLoanPeriodOverrideInput {
    pub loan_id: String,
    pub period_start: String,
    pub amount: Option<String>,
    pub pause_interest: bool,
    pub reason: String,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct ReverseLoanPostingInput {
    pub loan_id: String,
    pub posting_id: String,
    pub reason: String,
    pub reconciliation_reference: String,
    pub review_fingerprint: Option<String>,
    pub meta: LoanCommandMetaInput,
}
#[derive(InputObject)]
pub struct RetireLoanPolicyInput {
    pub policy_id: String,
    pub reason: String,
    pub meta: LoanCommandMetaInput,
}

#[derive(async_graphql::SimpleObject)]
#[graphql(name = "LoanCorrectionPreview")]
pub struct LoanCorrectionPreviewDto {
    pub loan_id: String,
    pub posting_id: String,
    pub account_version: i64,
    pub occurrence_date: String,
    pub posting_date: String,
    pub currency: String,
    pub current_principal: String,
    pub current_interest: String,
    pub corrected_principal: String,
    pub corrected_interest: String,
    pub preserved_payroll_postings: Vec<String>,
    pub review_fingerprint: String,
}
impl From<kabipay_loans::LoanCorrectionPreview> for LoanCorrectionPreviewDto {
    fn from(v: kabipay_loans::LoanCorrectionPreview) -> Self {
        Self {
            loan_id: v.loan_id.to_string(),
            posting_id: v.posting_id.to_string(),
            account_version: v.account_version,
            occurrence_date: v.occurrence_date.to_string(),
            posting_date: v.posting_date.to_string(),
            currency: v.currency,
            current_principal: v.current_principal.to_string(),
            current_interest: v.current_interest.to_string(),
            corrected_principal: v.corrected_principal.to_string(),
            corrected_interest: v.corrected_interest.to_string(),
            preserved_payroll_postings: v
                .preserved_payroll_postings
                .into_iter()
                .map(|id| id.to_string())
                .collect(),
            review_fingerprint: v.review_fingerprint,
        }
    }
}
