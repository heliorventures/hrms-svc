//! GraphQL DTOs for kabipay-leave.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_db_entities::tenant::d0011_leave::{leave_balance, leave_policy, leave_request, leave_type};
use kabipay_db_entities::tenant::d0025_workflow::workflow_action;

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveType")]
pub struct LeaveTypeDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: String,
    pub is_paid: bool,
    pub carry_forward: bool,
    pub max_carry_forward_days: Option<i32>,
    pub sandwich_rule: bool,
    pub half_day_allowed: bool,
    pub requires_document: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<leave_type::Model> for LeaveTypeDto {
    fn from(m: leave_type::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            code: m.code,
            is_paid: m.is_paid,
            carry_forward: m.carry_forward,
            max_carry_forward_days: m.max_carry_forward_days,
            sandwich_rule: m.sandwich_rule,
            half_day_allowed: m.half_day_allowed,
            requires_document: m.requires_document,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeavePolicy")]
pub struct LeavePolicyDto {
    pub id: ID,
    pub tenant_id: ID,
    pub leave_type_id: ID,
    pub applicable_to: Option<String>,
    pub annual_entitlement: Option<i32>,
    pub accrual_frequency: Option<String>,
    pub accrual_days: Option<String>,
    pub max_consecutive_days: Option<i32>,
    pub min_notice_days: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<leave_policy::Model> for LeavePolicyDto {
    fn from(m: leave_policy::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            leave_type_id: ID(m.leave_type_id.to_string()),
            applicable_to: m.applicable_to,
            annual_entitlement: m.annual_entitlement,
            accrual_frequency: m.accrual_frequency,
            accrual_days: m.accrual_days.map(|d| d.to_string()),
            max_consecutive_days: m.max_consecutive_days,
            min_notice_days: m.min_notice_days,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveWorkflowAction")]
pub struct LeaveWorkflowActionDto {
    pub workflow_step_name: String,
    pub action: String,
    pub remarks: Option<String>,
    pub acted_at: DateTime<Utc>,
    pub performed_by_user_id: Option<ID>,
}

impl LeaveWorkflowActionDto {
    pub fn from_action(step_name: String, a: workflow_action::Model) -> Self {
        Self {
            workflow_step_name: step_name,
            action: a.action,
            remarks: a.remarks,
            acted_at: a.acted_at,
            performed_by_user_id: a.performed_by.map(|u| ID(u.to_string())),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveRequest")]
pub struct LeaveRequestDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub employee_name: Option<String>,
    pub employee_code: Option<String>,
    pub leave_type_id: ID,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    /// Days requested, serialised as a decimal string for lossless transport.
    pub days_requested: String,
    pub is_half_day: bool,
    pub half_day_session: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub rejection_reason: Option<String>,
    /// Link or reference ID when the leave type requires documentation.
    pub supporting_document_reference: Option<String>,
    pub applied_at: DateTime<Utc>,
    /// Set when tenant has an active **LEAVE_REQUEST** workflow with at least one step (M8).
    pub workflow_instance_id: Option<ID>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveBalance")]
pub struct LeaveBalanceDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub leave_type_id: ID,
    pub year: i32,
    pub entitled_days: String,
    pub used_days: String,
    pub pending_days: String,
    pub carried_forward_days: String,
    pub balance_days: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<leave_balance::Model> for LeaveBalanceDto {
    fn from(m: leave_balance::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            leave_type_id: ID(m.leave_type_id.to_string()),
            year: m.year,
            entitled_days: m.entitled_days.to_string(),
            used_days: m.used_days.to_string(),
            pending_days: m.pending_days.to_string(),
            carried_forward_days: m.carried_forward_days.to_string(),
            balance_days: m.balance_days.to_string(),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

impl LeaveRequestDto {
    pub fn with_employee_label(mut self, name: String, code: String) -> Self {
        self.employee_name = Some(name);
        self.employee_code = Some(code);
        self
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct SubmitLeaveRequestInput {
    pub leave_type_id: ID,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub is_half_day: bool,
    pub half_day_session: Option<String>,
    pub reason: Option<String>,
    pub supporting_document_reference: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertLeaveTypeInput {
    pub id: Option<ID>,
    pub name: String,
    pub code: String,
    pub is_paid: bool,
    pub carry_forward: bool,
    pub max_carry_forward_days: Option<i32>,
    pub sandwich_rule: bool,
    pub half_day_allowed: bool,
    pub requires_document: bool,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertLeavePolicyInput {
    pub id: Option<ID>,
    pub leave_type_id: ID,
    pub applicable_to: Option<String>,
    pub annual_entitlement: Option<i32>,
    pub accrual_frequency: Option<String>,
    pub accrual_days: Option<String>,
    pub max_consecutive_days: Option<i32>,
    pub min_notice_days: Option<i32>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertLeaveBalanceInput {
    pub employee_id: ID,
    pub leave_type_id: ID,
    pub year: i32,
    pub entitled_days: String,
    pub used_days: String,
    pub pending_days: String,
    pub carried_forward_days: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct AdjustLeaveBalanceEntitlementInput {
    pub employee_id: ID,
    pub leave_type_id: ID,
    pub year: i32,
    pub entitled_delta: String,
    /// When true, adds `entitled_delta` to **balance_days** as well as **entitled_days** (simple grant).
    /// When false, recomputes **balance_days** from entitled / carried / used / pending.
    pub also_credit_balance: bool,
}

impl From<leave_request::Model> for LeaveRequestDto {
    fn from(m: leave_request::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            employee_name: None,
            employee_code: None,
            leave_type_id: ID(m.leave_type_id.to_string()),
            from_date: m.from_date,
            to_date: m.to_date,
            days_requested: m.days_requested.to_string(),
            is_half_day: m.is_half_day,
            half_day_session: m.half_day_session,
            status: m.status,
            reason: m.reason,
            rejection_reason: m.rejection_reason,
            supporting_document_reference: m.supporting_document_reference,
            applied_at: m.applied_at,
            workflow_instance_id: m.workflow_instance_id.map(|u| ID(u.to_string())),
        }
    }
}
