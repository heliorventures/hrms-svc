//! GraphQL DTOs for kabipay-expense.

use async_graphql::{ComplexObject, Context, InputObject, Result, SimpleObject, ID};
use kabipay_common::{
    context::{ClientClaims, PERM_EXPENSE_APPROVE, PERM_TRAVEL_APPROVE},
    subgraph::{require_tenant_id, tenant_db},
    KabiPayError,
};
use chrono::{DateTime, NaiveDate, Utc};

use crate::resolvers::query::parse_uuid;
use crate::services::approval_authority::{
    self, ApprovalSnapshot, ExpenseApprovalAuthority,
};
use crate::services::{expense_service, travel_request_service};
use kabipay_db_entities::tenant::d0015_expense::{expense, expense_category, expense_policy};
use std::{future::Future, sync::Arc};
use tokio::sync::OnceCell;

#[derive(Clone, Debug, Default)]
struct ApprovalSnapshotCache {
    value: Arc<OnceCell<ApprovalSnapshot>>,
}

impl ApprovalSnapshotCache {
    async fn get_or_try_init<E, F, Fut>(&self, init: F) -> std::result::Result<ApprovalSnapshot, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<ApprovalSnapshot, E>>,
    {
        self.value.get_or_try_init(init).await.cloned()
    }
}

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
    #[graphql(skip)]
    approval_snapshot_cache: ApprovalSnapshotCache,
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
    #[graphql(skip)]
    approval_snapshot_cache: ApprovalSnapshotCache,
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
            approval_snapshot_cache: ApprovalSnapshotCache::default(),
        }
    }
}

impl ExpenseDto {
    async fn approval_snapshot(&self, ctx: &Context<'_>) -> Result<ApprovalSnapshot> {
        self.approval_snapshot_cache
            .get_or_try_init(|| async {
                let tenant_id = require_tenant_id(ctx)?;
                let db = tenant_db(ctx, tenant_id).await?;
                let authority = ctx
                    .data_opt::<ClientClaims>()
                    .and_then(|claims| {
                        ExpenseApprovalAuthority::from_claims(claims, PERM_EXPENSE_APPROVE).ok()
                    });
                approval_authority::resolve_approval_snapshot(
                    &db,
                    tenant_id,
                    expense_service::WF_ENTITY_EXPENSE,
                    parse_uuid(&self.id, "expenseId")?,
                    &self.status,
                    parse_uuid(&self.employee_id, "employeeId")?,
                    self.workflow_instance_id
                        .as_ref()
                        .map(|id| parse_uuid(id, "workflowInstanceId"))
                        .transpose()?,
                    authority.as_ref(),
                )
                .await
                .map_err(KabiPayError::into_graphql)
            })
            .await
    }
}

#[ComplexObject]
impl ExpenseDto {
    async fn pending_approval_stage(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        Ok(self.approval_snapshot(ctx).await?.pending_stage)
    }

    async fn viewer_may_approve(&self, ctx: &Context<'_>) -> Result<bool> {
        Ok(self.approval_snapshot(ctx).await?.actionable_step_id.is_some())
    }

    async fn pending_approval_step_id(&self, ctx: &Context<'_>) -> Result<Option<ID>> {
        Ok(self
            .approval_snapshot(ctx)
            .await?
            .actionable_step_id
            .map(|id| ID(id.to_string())))
    }
}

impl TravelRequestDto {
    async fn approval_snapshot(&self, ctx: &Context<'_>) -> Result<ApprovalSnapshot> {
        self.approval_snapshot_cache
            .get_or_try_init(|| async {
                let tenant_id = require_tenant_id(ctx)?;
                let db = tenant_db(ctx, tenant_id).await?;
                let authority = ctx.data_opt::<ClientClaims>().and_then(|claims| {
                    ExpenseApprovalAuthority::from_claims(claims, PERM_TRAVEL_APPROVE).ok()
                });
                approval_authority::resolve_approval_snapshot(
                    &db,
                    tenant_id,
                    travel_request_service::WF_ENTITY_TRAVEL_REQUEST,
                    parse_uuid(&self.id, "travelRequestId")?,
                    &self.status,
                    parse_uuid(&self.employee_id, "employeeId")?,
                    self.workflow_instance_id
                        .as_ref()
                        .map(|id| parse_uuid(id, "workflowInstanceId"))
                        .transpose()?,
                    authority.as_ref(),
                )
                .await
                .map_err(KabiPayError::into_graphql)
            })
            .await
    }
}

#[ComplexObject]
impl TravelRequestDto {
    async fn pending_approval_stage(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        Ok(self.approval_snapshot(ctx).await?.pending_stage)
    }

    async fn viewer_may_approve(&self, ctx: &Context<'_>) -> Result<bool> {
        Ok(self.approval_snapshot(ctx).await?.actionable_step_id.is_some())
    }

    async fn pending_approval_step_id(&self, ctx: &Context<'_>) -> Result<Option<ID>> {
        Ok(self
            .approval_snapshot(ctx)
            .await?
            .actionable_step_id
            .map(|id| ID(id.to_string())))
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
            approval_snapshot_cache: ApprovalSnapshotCache::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uuid::Uuid;

    struct TestQuery;

    #[Object]
    impl TestQuery {
        async fn expense(&self) -> ExpenseDto {
            ExpenseDto {
                id: ID(Uuid::new_v4().to_string()),
                tenant_id: ID(Uuid::new_v4().to_string()),
                employee_id: ID(Uuid::new_v4().to_string()),
                expense_category_id: ID(Uuid::new_v4().to_string()),
                travel_request_id: None,
                amount: "100".into(),
                currency: "INR".into(),
                expense_date: NaiveDate::from_ymd_opt(2026, 8, 27).expect("valid date"),
                title: "Taxi".into(),
                status: "PENDING".into(),
                workflow_instance_id: Some(ID(Uuid::new_v4().to_string())),
                submitted_at: Utc::now(),
                approved_amount: None,
                payment_status: "NONE".into(),
                paid_at: None,
                payment_reference: None,
                receipt_file_storage_id: None,
                approval_snapshot_cache: ApprovalSnapshotCache::default(),
            }
        }

        async fn travel_request(&self) -> TravelRequestDto {
            TravelRequestDto {
                id: ID(Uuid::new_v4().to_string()),
                tenant_id: ID(Uuid::new_v4().to_string()),
                employee_id: ID(Uuid::new_v4().to_string()),
                origin_location: None,
                destination_location: None,
                from_date: NaiveDate::from_ymd_opt(2026, 8, 27).expect("valid date"),
                to_date: NaiveDate::from_ymd_opt(2026, 8, 28).expect("valid date"),
                purpose: "Client visit".into(),
                estimated_amount: None,
                currency: "INR".into(),
                status: "PENDING".into(),
                rejection_reason: None,
                approved_by: None,
                rejected_by: None,
                workflow_instance_id: Some(ID(Uuid::new_v4().to_string())),
                submitted_at: Utc::now(),
                approval_snapshot_cache: ApprovalSnapshotCache::default(),
            }
        }
    }

    #[test]
    fn expense_and_travel_schema_expose_server_authoritative_approval_step_tokens() {
        let schema = Schema::build(TestQuery, EmptyMutation, EmptySubscription).finish();
        let sdl = schema.sdl();
        for graphql_type in ["Expense", "TravelRequest"] {
            let type_start = sdl
                .find(&format!("type {graphql_type} {{"))
                .expect("GraphQL type exists");
            let type_body = &sdl[type_start..];
            let type_end = type_body.find('}').expect("GraphQL type closes");
            let type_body = &type_body[..type_end];
            assert!(
                type_body.contains("pendingApprovalStepId: ID"),
                "{graphql_type} must expose the current actionable workflow step token: {type_body}"
            );
        }
    }

    #[tokio::test]
    async fn approval_snapshot_cache_shares_one_concurrent_lookup() {
        let cache = ApprovalSnapshotCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let step_id = Uuid::new_v4();
        let expected = crate::services::approval_authority::ApprovalSnapshot {
            pending_stage: Some("Manager approval".into()),
            actionable_step_id: Some(step_id),
        };
        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let first_expected = expected.clone();
        let second_expected = expected.clone();

        let (first, second) = tokio::join!(
            cache.get_or_try_init(|| async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok::<_, KabiPayError>(first_expected)
            }),
            cache.get_or_try_init(|| async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, KabiPayError>(second_expected)
            })
        );

        assert_eq!(first.expect("first approval snapshot"), expected);
        assert_eq!(second.expect("second approval snapshot"), expected);
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }
}
