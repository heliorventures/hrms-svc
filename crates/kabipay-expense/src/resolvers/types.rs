//! GraphQL DTOs for kabipay-expense.

use async_graphql::{ComplexObject, Context, InputObject, Result, SimpleObject, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use chrono::{DateTime, NaiveDate, Utc};

use crate::resolvers::query::parse_uuid;
use crate::services::{expense_service, travel_request_service};
use kabipay_db_entities::tenant::d0015_expense::{expense, expense_category, expense_policy};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ExpenseCategory")]
pub struct ExpenseCategoryDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: String,
    pub max_amount_per_claim: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<expense_category::Model> for ExpenseCategoryDto {
    fn from(m: expense_category::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            code: m.code,
            max_amount_per_claim: m.max_amount_per_claim.map(|d| d.to_string()),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(complex)]
#[graphql(name = "Expense")]
pub struct ExpenseDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub expense_category_id: ID,
    /// When set, this claim is part of a travel trip.
    pub travel_request_id: Option<ID>,
    pub amount: String,
    pub currency: String,
    pub expense_date: NaiveDate,
    pub title: String,
    pub status: String,
    /// Set when **`EXPENSE`** workflow is active with ≥1 step (**M32**).
    pub workflow_instance_id: Option<ID>,
    pub submitted_at: DateTime<Utc>,
    pub approved_amount: Option<String>,
    pub payment_status: String,
    pub paid_at: Option<DateTime<Utc>>,
    pub payment_reference: Option<String>,
    pub receipt_file_storage_id: Option<ID>,
}

#[derive(InputObject, Clone, Debug)]
pub struct SubmitExpenseInput {
    pub expense_category_id: ID,
    /// String decimal, e.g. "1250.50"
    pub amount: String,
    /// ISO 4217, e.g. "INR"
    pub currency: String,
    pub expense_date: NaiveDate,
    pub title: String,
    /// Link to a travel request the employee owns (optional).
    pub travel_request_id: Option<ID>,
    pub receipt_file_storage_id: Option<ID>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(complex)]
#[graphql(name = "TravelRequest")]
pub struct TravelRequestDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub origin_location: Option<String>,
    pub destination_location: Option<String>,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub purpose: String,
    pub estimated_amount: Option<String>,
    pub currency: String,
    pub status: String,
    pub rejection_reason: Option<String>,
    pub approved_by: Option<ID>,
    pub rejected_by: Option<ID>,
    /// Present when **`TRAVEL_REQUEST`** workflow is active (**M32** parity with expenses).
    pub workflow_instance_id: Option<ID>,
    pub submitted_at: DateTime<Utc>,
}

#[derive(InputObject, Clone, Debug)]
pub struct SubmitTravelRequestInput {
    pub origin_location: Option<String>,
    pub destination_location: Option<String>,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub purpose: String,
    /// Optional string decimal; omit for unknown estimate.
    pub estimated_amount: Option<String>,
    pub currency: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertExpenseCategoryAdminInput {
    /// When **`None`**, creates a category; otherwise updates that tenant row.
    pub id: Option<ID>,
    pub name: String,
    pub code: String,
    /// Optional decimal string ceiling per claim; omit/`null`/empty clears the cap.
    pub max_amount_per_claim: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ExpensePolicy")]
pub struct ExpensePolicyDto {
    pub id: ID,
    pub tenant_id: ID,
    pub expense_category_id: ID,
    pub applicable_to: String,
    pub department_id: Option<ID>,
    pub designation_id: Option<ID>,
    pub role_id: Option<ID>,
    pub limit_per_day: Option<String>,
    pub limit_per_month: Option<String>,
    pub max_amount_per_claim: Option<String>,
    pub receipt_required: bool,
    pub approval_required: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<expense_policy::Model> for ExpensePolicyDto {
    fn from(m: expense_policy::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            expense_category_id: ID(m.expense_category_id.to_string()),
            applicable_to: m.applicable_to,
            department_id: m.department_id.map(|u| ID(u.to_string())),
            designation_id: m.designation_id.map(|u| ID(u.to_string())),
            role_id: m.role_id.map(|u| ID(u.to_string())),
            limit_per_day: m.limit_per_day.map(|d| d.to_string()),
            limit_per_month: m.limit_per_month.map(|d| d.to_string()),
            max_amount_per_claim: m.max_amount_per_claim.map(|d| d.to_string()),
            receipt_required: m.receipt_required,
            approval_required: m.approval_required,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertExpensePolicyAdminInput {
    pub id: Option<ID>,
    pub expense_category_id: ID,
    pub applicable_to: String,
    pub department_id: Option<ID>,
    pub designation_id: Option<ID>,
    pub role_id: Option<ID>,
    pub limit_per_day: Option<String>,
    pub limit_per_month: Option<String>,
    pub max_amount_per_claim: Option<String>,
    pub receipt_required: bool,
    pub approval_required: bool,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct ExpenseSubmissionHints {
    pub expense_category_id: ID,
    pub max_amount_per_claim: Option<String>,
    pub receipt_required: bool,
    pub limit_per_month: Option<String>,
    pub limit_per_day: Option<String>,
}

impl From<kabipay_db_entities::tenant::d0033_travel_request::travel_request::Model> for TravelRequestDto {
    fn from(m: kabipay_db_entities::tenant::d0033_travel_request::travel_request::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            origin_location: m.origin_location,
            destination_location: m.destination_location,
            from_date: m.from_date,
            to_date: m.to_date,
            purpose: m.purpose,
            estimated_amount: m.estimated_amount.map(|d| d.to_string()),
            currency: m.currency,
            status: m.status,
            rejection_reason: m.rejection_reason,
            approved_by: m.approved_by.map(|u| ID(u.to_string())),
            rejected_by: m.rejected_by.map(|u| ID(u.to_string())),
            workflow_instance_id: m.workflow_instance_id.map(|u| ID(u.to_string())),
            submitted_at: m.submitted_at,
        }
    }
}

#[ComplexObject]
impl ExpenseDto {
    async fn pending_approval_stage(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        expense_service::resolve_expense_pending_approval_stage(&db, tenant_id, &self.status, wf)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn viewer_may_approve(&self, ctx: &Context<'_>) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let claims = require_client_claims(ctx)?;
        let employee_id = parse_uuid(&self.employee_id, "employeeId")?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        expense_service::expense_viewer_may_approve(
            &db,
            tenant_id,
            claims.sub,
            claims.can_approve_expense(),
            &self.status,
            employee_id,
            wf,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }
}

#[ComplexObject]
impl TravelRequestDto {
    async fn pending_approval_stage(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        travel_request_service::resolve_travel_pending_approval_stage(&db, tenant_id, &self.status, wf)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn viewer_may_approve(&self, ctx: &Context<'_>) -> Result<bool> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let claims = require_client_claims(ctx)?;
        let employee_id = parse_uuid(&self.employee_id, "employeeId")?;
        let wf = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        travel_request_service::travel_viewer_may_approve(
            &db,
            tenant_id,
            claims.sub,
            claims.can_approve_expense(),
            &self.status,
            employee_id,
            wf,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }
}

impl From<expense::Model> for ExpenseDto {
    fn from(m: expense::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            expense_category_id: ID(m.expense_category_id.to_string()),
            travel_request_id: m.travel_request_id.map(|u| ID(u.to_string())),
            amount: m.amount.to_string(),
            currency: m.currency,
            expense_date: m.expense_date,
            title: m.title,
            status: m.status,
            workflow_instance_id: m.workflow_instance_id.map(|u| ID(u.to_string())),
            submitted_at: m.submitted_at,
            approved_amount: m.approved_amount.map(|d| d.to_string()),
            payment_status: m.payment_status,
            paid_at: m.paid_at,
            payment_reference: m.payment_reference,
            receipt_file_storage_id: m.receipt_file_storage_id.map(|u| ID(u.to_string())),
        }
    }
}
