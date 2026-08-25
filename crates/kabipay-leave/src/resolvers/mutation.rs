//! Write operations for the leave domain.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{data_scope_from_context, resolve_viewer_employee},
    context::PERM_LEAVE_APPROVE,
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    workflow_approval::WorkflowApprovalAuthority,
    KabiPayError,
};
use rust_decimal::Decimal;
use std::str::FromStr;

use crate::resolvers::query::parse_uuid;
use crate::resolvers::types::{
    AdjustLeaveBalanceEntitlementInput, LeaveBalanceDto, LeavePolicyDto, LeaveRequestDto,
    LeaveTypeDto, SubmitLeaveRequestInput, UpsertLeaveBalanceInput, UpsertLeavePolicyInput,
    UpsertLeaveTypeInput,
};
use crate::services::{leave_admin, leave_service};

pub struct MutationRoot;

fn parse_dec(raw: &str, field: &'static str) -> Result<Decimal> {
    Decimal::from_str(raw.trim()).map_err(|e| {
        KabiPayError::Validation(format!("invalid {field}: {e}"))
            .into_graphql()
    })
}

fn require_leave_admin(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_leave_configuration() {
        return Err(
            KabiPayError::Forbidden("missing permission to manage leave configuration".into())
                .into_graphql(),
        );
    }
    Ok(())
}

#[Object]
impl MutationRoot {
    /// Create a PENDING leave request and reserve days against the annual balance.
    async fn submit_leave_request(
        &self,
        ctx: &Context<'_>,
        input: SubmitLeaveRequestInput,
    ) -> Result<LeaveRequestDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let leave_type_id = parse_uuid(&input.leave_type_id, "leaveTypeId")?;
        let m = leave_service::submit_leave_request(
            &db,
            tenant_id,
            employee_id,
            leave_type_id,
            input.from_date,
            input.to_date,
            input.is_half_day,
            input.half_day_session,
            input.reason,
            input.supporting_document_reference,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveRequestDto::from(m))
    }

    /// Set a PENDING request to APPROVED and credit used leave (see `submit_leave_request` balance flow).
    async fn approve_leave_request(
        &self,
        ctx: &Context<'_>,
        leave_request_id: ID,
    ) -> Result<LeaveRequestDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope: data_scope_from_context(ctx, PERM_LEAVE_APPROVE)?,
            permission: PERM_LEAVE_APPROVE,
        };
        let id = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let m = leave_service::approve_leave_request(&db, tenant_id, id, &authority)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveRequestDto::from(m))
    }

    /// Reject a PENDING request and release the balance reservation.
    async fn reject_leave_request(
        &self,
        ctx: &Context<'_>,
        leave_request_id: ID,
        reason: Option<String>,
    ) -> Result<LeaveRequestDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let authority = WorkflowApprovalAuthority {
            actor_user_id: claims.sub,
            actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
            scope: data_scope_from_context(ctx, PERM_LEAVE_APPROVE)?,
            permission: PERM_LEAVE_APPROVE,
        };
        let id = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let m = leave_service::reject_leave_request(&db, tenant_id, id, &authority, reason)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveRequestDto::from(m))
    }

    /// Withdraw own **PENDING** leave request (releases balance hold; cancels workflow when present).
    async fn cancel_leave_request(
        &self,
        ctx: &Context<'_>,
        leave_request_id: ID,
    ) -> Result<LeaveRequestDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let id = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let m = leave_service::cancel_leave_request(&db, tenant_id, id, employee_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveRequestDto::from(m))
    }

    async fn upsert_leave_type(
        &self,
        ctx: &Context<'_>,
        input: UpsertLeaveTypeInput,
    ) -> Result<LeaveTypeDto> {
        require_leave_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = input.id.as_ref().map(|i| parse_uuid(i, "leaveTypeId")).transpose()?;
        let m = leave_admin::upsert_leave_type(
            &db,
            tenant_id,
            id,
            input.name,
            input.code,
            input.is_paid,
            input.carry_forward,
            input.max_carry_forward_days,
            input.sandwich_rule,
            input.half_day_allowed,
            input.requires_document,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveTypeDto::from(m))
    }

    async fn delete_leave_type(&self, ctx: &Context<'_>, leave_type_id: ID) -> Result<LeaveTypeDto> {
        require_leave_admin(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&leave_type_id, "leaveTypeId")?;
        let deleted_by = Some(claims.sub);
        let m = leave_admin::soft_delete_leave_type(&db, tenant_id, id, deleted_by)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveTypeDto::from(m))
    }

    async fn upsert_leave_policy(
        &self,
        ctx: &Context<'_>,
        input: UpsertLeavePolicyInput,
    ) -> Result<LeavePolicyDto> {
        require_leave_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = input.id.as_ref().map(|i| parse_uuid(i, "leavePolicyId")).transpose()?;
        let lt = parse_uuid(&input.leave_type_id, "leaveTypeId")?;
        let accrual_days = match &input.accrual_days {
            None => None,
            Some(s) if s.trim().is_empty() => None,
            Some(s) => Some(parse_dec(s, "accrualDays")?),
        };
        let m = leave_admin::upsert_leave_policy(
            &db,
            tenant_id,
            id,
            lt,
            input.applicable_to,
            input.annual_entitlement,
            input.accrual_frequency,
            accrual_days,
            input.max_consecutive_days,
            input.min_notice_days,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(LeavePolicyDto::from(m))
    }

    async fn delete_leave_policy(&self, ctx: &Context<'_>, leave_policy_id: ID) -> Result<bool> {
        require_leave_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&leave_policy_id, "leavePolicyId")?;
        leave_admin::delete_leave_policy(&db, tenant_id, id)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn upsert_leave_balance(
        &self,
        ctx: &Context<'_>,
        input: UpsertLeaveBalanceInput,
    ) -> Result<LeaveBalanceDto> {
        require_leave_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = parse_uuid(&input.employee_id, "employeeId")?;
        let lt = parse_uuid(&input.leave_type_id, "leaveTypeId")?;
        let entitled = parse_dec(&input.entitled_days, "entitledDays")?;
        let used = parse_dec(&input.used_days, "usedDays")?;
        let pending = parse_dec(&input.pending_days, "pendingDays")?;
        let carried = parse_dec(&input.carried_forward_days, "carriedForwardDays")?;
        let m = leave_admin::upsert_leave_balance(
            &db,
            tenant_id,
            emp,
            lt,
            input.year,
            entitled,
            used,
            pending,
            carried,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveBalanceDto::from(m))
    }

    async fn adjust_leave_balance_entitlement(
        &self,
        ctx: &Context<'_>,
        input: AdjustLeaveBalanceEntitlementInput,
    ) -> Result<LeaveBalanceDto> {
        require_leave_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = parse_uuid(&input.employee_id, "employeeId")?;
        let lt = parse_uuid(&input.leave_type_id, "leaveTypeId")?;
        let delta = parse_dec(&input.entitled_delta, "entitledDelta")?;
        let m = leave_admin::adjust_leave_balance_entitlement(
            &db,
            tenant_id,
            emp,
            lt,
            input.year,
            delta,
            input.also_credit_balance,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveBalanceDto::from(m))
    }

    /// Upsert **leave_balance** rows for **all** active employees from published leave policies
    /// (annual entitlement, or MONTHLY accrual × 12). Returns how many employee/type/year rows were written.
    async fn provision_leave_balances_from_policies(
        &self,
        ctx: &Context<'_>,
        year: i32,
    ) -> Result<i32> {
        require_leave_admin(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let n = leave_admin::provision_leave_balances_from_policies(&db, tenant_id, year)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(n as i32)
    }
}
