//! Write operations for expense claims.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    ExpenseCategoryDto, ExpenseDto, ExpensePolicyDto, SubmitExpenseInput, SubmitTravelRequestInput,
    TravelRequestDto, UpsertExpenseCategoryAdminInput, UpsertExpensePolicyAdminInput,
};
use crate::services::{expense_service, travel_request_service};

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn require_expense_admin(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_expense_configuration() {
        return Err(
            KabiPayError::Forbidden("missing permission to manage expense categories".into())
                .into_graphql(),
        );
    }
    Ok(())
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    /// Create a PENDING expense claim for the signed-in user’s employee record.
    async fn submit_expense(
        &self,
        ctx: &Context<'_>,
        input: SubmitExpenseInput,
    ) -> Result<ExpenseDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let category_id = parse_uuid(&input.expense_category_id, "expenseCategoryId")?;
        let amount =
            expense_service::parse_amount(&input.amount).map_err(KabiPayError::into_graphql)?;
        let opt_travel = if let Some(tid) = &input.travel_request_id {
            Some(parse_uuid(tid, "travelRequestId")?)
        } else {
            None
        };
        let receipt = if let Some(rid) = &input.receipt_file_storage_id {
            Some(parse_uuid(rid, "receiptFileStorageId")?)
        } else {
            None
        };
        let m = expense_service::submit_expense(
            &db,
            tenant_id,
            employee_id,
            claims.sub,
            category_id,
            amount,
            &input.currency,
            input.expense_date,
            &input.title,
            opt_travel,
            receipt,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }

    async fn approve_expense(
        &self,
        ctx: &Context<'_>,
        expense_id: ID,
        approved_amount: Option<String>,
    ) -> Result<ExpenseDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_id, "expenseId")?;
        let approved_dec = match approved_amount {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(
                expense_service::parse_amount(&s).map_err(KabiPayError::into_graphql)?,
            ),
        };
        let m = expense_service::approve_expense(
            &db,
            tenant_id,
            id,
            claims.sub,
            claims.can_approve_expense(),
            approved_dec,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }

    async fn reject_expense(
        &self,
        ctx: &Context<'_>,
        expense_id: ID,
        reason: Option<String>,
    ) -> Result<ExpenseDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_id, "expenseId")?;
        let m = expense_service::reject_expense(
            &db,
            tenant_id,
            id,
            claims.sub,
            claims.can_approve_expense(),
            reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }

    /// Create a **PENDING** travel request for the signed-in employee.
    async fn submit_travel_request(
        &self,
        ctx: &Context<'_>,
        input: SubmitTravelRequestInput,
    ) -> Result<TravelRequestDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let est = match &input.estimated_amount {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };
        let currency = if input.currency.trim().is_empty() {
            "INR"
        } else {
            input.currency.trim()
        };
        let m = travel_request_service::submit_travel_request(
            &db,
            tenant_id,
            employee_id,
            input.origin_location,
            input.destination_location,
            input.from_date,
            input.to_date,
            &input.purpose,
            est,
            currency,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TravelRequestDto::from(m))
    }

    async fn approve_travel_request(
        &self,
        ctx: &Context<'_>,
        travel_request_id: ID,
    ) -> Result<TravelRequestDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&travel_request_id, "travelRequestId")?;
        let m = travel_request_service::approve_travel_request(
            &db,
            tenant_id,
            id,
            claims.sub,
            claims.can_approve_expense(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TravelRequestDto::from(m))
    }

    async fn reject_travel_request(
        &self,
        ctx: &Context<'_>,
        travel_request_id: ID,
        reason: Option<String>,
    ) -> Result<TravelRequestDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&travel_request_id, "travelRequestId")?;
        let m = travel_request_service::reject_travel_request(
            &db,
            tenant_id,
            id,
            claims.sub,
            claims.can_approve_expense(),
            reason,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(TravelRequestDto::from(m))
    }

    /// Create or update an **`expense_category`** row (**`expense:manage`** required).
    async fn upsert_expense_category_admin(
        &self,
        ctx: &Context<'_>,
        input: UpsertExpenseCategoryAdminInput,
    ) -> Result<ExpenseCategoryDto> {
        require_expense_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = match &input.id {
            Some(raw) => Some(parse_uuid(raw, "categoryId")?),
            None => None,
        };
        let max_amt = match &input.max_amount_per_claim {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(
                expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?,
            ),
        };
        let m = expense_service::upsert_expense_category(
            &db,
            tenant_id,
            id,
            &input.name,
            &input.code,
            max_amt,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseCategoryDto::from(m))
    }

    /// Soft-delete an **`expense_category`** (**`expense:manage`** required).
    async fn delete_expense_category_admin(
        &self,
        ctx: &Context<'_>,
        expense_category_id: ID,
    ) -> Result<bool> {
        require_expense_admin(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_category_id, "expenseCategoryId")?;
        expense_service::delete_expense_category(&db, tenant_id, claims.sub, id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn upsert_expense_policy_admin(
        &self,
        ctx: &Context<'_>,
        input: UpsertExpensePolicyAdminInput,
    ) -> Result<ExpensePolicyDto> {
        require_expense_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = match &input.id {
            Some(raw) => Some(parse_uuid(raw, "policyId")?),
            None => None,
        };
        let cat = parse_uuid(&input.expense_category_id, "expenseCategoryId")?;
        let department_id = match &input.department_id {
            None => None,
            Some(raw) => Some(parse_uuid(raw, "departmentId")?),
        };
        let designation_id = match &input.designation_id {
            None => None,
            Some(raw) => Some(parse_uuid(raw, "designationId")?),
        };
        let role_id = match &input.role_id {
            None => None,
            Some(raw) => Some(parse_uuid(raw, "roleId")?),
        };
        let limit_day = match &input.limit_per_day {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };
        let limit_month = match &input.limit_per_month {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };
        let max_claim = match &input.max_amount_per_claim {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(expense_service::parse_amount(s).map_err(KabiPayError::into_graphql)?),
        };

        let m = expense_service::upsert_expense_policy_admin(
            &db,
            tenant_id,
            id,
            cat,
            &input.applicable_to,
            department_id,
            designation_id,
            role_id,
            limit_day,
            limit_month,
            max_claim,
            input.receipt_required,
            input.approval_required,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpensePolicyDto::from(m))
    }

    async fn delete_expense_policy_admin(
        &self,
        ctx: &Context<'_>,
        expense_policy_id: ID,
    ) -> Result<bool> {
        require_expense_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_policy_id, "expensePolicyId")?;
        expense_service::delete_expense_policy_admin(&db, tenant_id, id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    /// Update reimbursement bookkeeping after financial approval (**`expense:pay`** or elevated approvers).
    async fn mark_expense_payment_status(
        &self,
        ctx: &Context<'_>,
        expense_id: ID,
        payment_status: String,
        payment_reference: Option<String>,
    ) -> Result<ExpenseDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_mark_expense_payment() {
            return Err(KabiPayError::Forbidden(
                "missing permission to update expense payment status".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&expense_id, "expenseId")?;
        let pref = payment_reference.as_deref();
        let m = expense_service::mark_expense_payment_status(
            &db,
            tenant_id,
            id,
            &payment_status,
            pref,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(ExpenseDto::from(m))
    }
}
