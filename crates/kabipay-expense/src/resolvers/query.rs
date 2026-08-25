//! Root query resolvers for kabipay-expense.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_context, resolve_employee_scope_filter, resolve_viewer_employee,
    },
    context::PERM_EXPENSE_READ,
    subgraph::{
        require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db,
    },
    KabiPayError,
};

use crate::resolvers::types::{
    ExpenseCategoryDto, ExpenseDto, ExpensePolicyDto, ExpenseSubmissionHints, TravelRequestDto,
};
use crate::services::{expense_service, travel_request_service};
use uuid::Uuid;

pub(crate) fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn require_expense_configuration(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_expense_configuration() {
        return Err(
            KabiPayError::Forbidden("missing permission to manage expense configuration".into())
                .into_graphql(),
        );
    }
    Ok(())
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn expense_health(&self) -> &'static str {
        "ok"
    }

    async fn expense_categories(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ExpenseCategoryDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = expense_service::list_categories(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ExpenseCategoryDto::from).collect())
    }

    async fn expenses(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ExpenseDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, PERM_EXPENSE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows = expense_service::list_expenses(&db, tenant_id, limit, &filt)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ExpenseDto::from).collect())
    }

    /// Travel / trip requests for the caller’s **expense** data scope (same as `expenses`).
    async fn travel_requests(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TravelRequestDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, PERM_EXPENSE_READ)?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rows = travel_request_service::list_travel_requests(&db, tenant_id, limit, &filt)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TravelRequestDto::from).collect())
    }

    async fn expense_submission_hints(
        &self,
        ctx: &Context<'_>,
        expense_category_id: ID,
    ) -> Result<ExpenseSubmissionHints> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let cid = parse_uuid(&expense_category_id, "expenseCategoryId")?;
        let cat =
            expense_service::get_expense_category(&db, tenant_id, cid).await?;

        let h = expense_service::resolve_expense_submit_constraints(
            &db,
            tenant_id,
            cid,
            employee_id,
            cat.max_amount_per_claim,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseSubmissionHints {
            expense_category_id,
            max_amount_per_claim: h.max_amount_per_claim.map(|d| d.to_string()),
            receipt_required: h.receipt_required,
            limit_per_month: h.limit_per_month.map(|d| d.to_string()),
            limit_per_day: h.limit_per_day.map(|d| d.to_string()),
        })
    }

    /// Scoped expense policies for a category (**`expense:manage`**).
    async fn expense_policies_for_admin(
        &self,
        ctx: &Context<'_>,
        expense_category_id: ID,
    ) -> Result<Vec<ExpensePolicyDto>> {
        require_expense_configuration(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&expense_category_id, "expenseCategoryId")?;
        let rows = expense_service::list_expense_policies_for_category(&db, tenant_id, cid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ExpensePolicyDto::from).collect())
    }
}
