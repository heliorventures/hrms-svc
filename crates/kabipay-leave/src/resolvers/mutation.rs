//! Write operations for the leave domain.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::data_scope_from_context,
    context::{ScopeType, PERM_LEAVE_APPROVE, PERM_LEAVE_MANAGE, PERM_LEAVE_SUBMIT},
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
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

fn require_exact_leave_scope(
    ctx: &Context<'_>,
    permission: &'static str,
    allowed: &[ScopeType],
) -> Result<ScopeType> {
    let scope = data_scope_from_context(ctx, permission)?;
    if !allowed.contains(&scope) {
        return Err(KabiPayError::Forbidden(format!(
            "{permission} permission does not have a suitable explicit scope"
        ))
        .into_graphql());
    }
    Ok(scope)
}

fn require_leave_submit(ctx: &Context<'_>) -> Result<()> {
    require_exact_leave_scope(ctx, PERM_LEAVE_SUBMIT, &[ScopeType::Self_]).map(|_| ())
}

fn leave_approval_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    require_exact_leave_scope(
        ctx,
        PERM_LEAVE_APPROVE,
        &[ScopeType::Team, ScopeType::All],
    )
}

fn require_leave_admin(ctx: &Context<'_>) -> Result<()> {
    require_exact_leave_scope(ctx, PERM_LEAVE_MANAGE, &[ScopeType::All]).map(|_| ())
}

#[Object]
impl MutationRoot {
    /// Create a PENDING leave request and reserve days against the annual balance.
    async fn submit_leave_request(
        &self,
        ctx: &Context<'_>,
        input: SubmitLeaveRequestInput,
    ) -> Result<LeaveRequestDto> {
        require_leave_submit(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let leave_type_id = parse_uuid(&input.leave_type_id, "leaveTypeId")?;
        let m = leave_service::submit_leave_request(
            &db,
            tenant_id,
            claims.sub,
            claims.employee_id,
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
        expected_workflow_step_id: ID,
    ) -> Result<LeaveRequestDto> {
        let claims = require_client_claims(ctx)?;
        let scope = leave_approval_scope(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let expected_step_id = parse_uuid(&expected_workflow_step_id, "expectedWorkflowStepId")?;
        let m = leave_service::approve_leave_request(
            &db,
            tenant_id,
            id,
            expected_step_id,
            claims.sub,
            claims.employee_id,
            scope,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(LeaveRequestDto::from(m))
    }

    /// Reject a PENDING request and release the balance reservation.
    async fn reject_leave_request(
        &self,
        ctx: &Context<'_>,
        leave_request_id: ID,
        expected_workflow_step_id: ID,
        reason: Option<String>,
    ) -> Result<LeaveRequestDto> {
        let claims = require_client_claims(ctx)?;
        let scope = leave_approval_scope(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let expected_step_id = parse_uuid(&expected_workflow_step_id, "expectedWorkflowStepId")?;
        let m = leave_service::reject_leave_request(
            &db,
            tenant_id,
            id,
            expected_step_id,
            claims.sub,
            claims.employee_id,
            scope,
            reason,
        )
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
        require_leave_submit(ctx)?;
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let m = leave_service::cancel_leave_request(
            &db,
            tenant_id,
            id,
            claims.sub,
            claims.employee_id,
        )
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
        let m = leave_service::upsert_leave_balance(
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
        let m = leave_service::adjust_leave_balance_entitlement(
            &db,
            tenant_id,
            emp,
            lt,
            input.year,
            delta,
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
        let n = leave_service::provision_leave_balances_from_policies(&db, tenant_id, year)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(n as i32)
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use async_graphql::{EmptySubscription, Object, Request, Schema};
    use kabipay_common::context::{
        ClientClaims, CLIENT_JWT_ISSUER, PERM_LEAVE_MANAGE, PERM_LEAVE_READ,
        PERM_LEAVE_SUBMIT,
    };
    use kabipay_common::subgraph::TenantId;
    use std::collections::HashMap;
    use uuid::Uuid;

    struct TestQuery;

    #[Object]
    impl TestQuery {
        async fn api_version(&self) -> &str {
            "test"
        }
    }

    fn claims(permission: Option<&str>, scope: Option<&str>) -> ClientClaims {
        let permissions = permission.into_iter().map(str::to_owned).collect();
        let permission_scopes = permission
            .zip(scope)
            .map(|(permission, scope)| {
                HashMap::from([(permission.to_owned(), scope.to_owned())])
            })
            .unwrap_or_default();
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: vec![],
            permissions,
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    async fn execute_mutation(
        claims: ClientClaims,
        mutation: &str,
    ) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(TestQuery, MutationRoot, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(mutation))
            .await
    }

    fn mutation_inventory() -> Vec<(&'static str, String)> {
        let id = Uuid::new_v4();
        vec![
            (
                PERM_LEAVE_SUBMIT,
                format!(
                    "mutation {{ submitLeaveRequest(input: {{ leaveTypeId: \"{id}\", fromDate: \"2026-08-27\", toDate: \"2026-08-27\", isHalfDay: false }}) {{ id }} }}"
                ),
            ),
            (
                PERM_LEAVE_APPROVE,
                format!(
                    "mutation {{ approveLeaveRequest(leaveRequestId: \"{id}\", expectedWorkflowStepId: \"{id}\") {{ id }} }}"
                ),
            ),
            (
                PERM_LEAVE_APPROVE,
                format!(
                    "mutation {{ rejectLeaveRequest(leaveRequestId: \"{id}\", expectedWorkflowStepId: \"{id}\", reason: \"invalid\") {{ id }} }}"
                ),
            ),
            (
                PERM_LEAVE_SUBMIT,
                format!("mutation {{ cancelLeaveRequest(leaveRequestId: \"{id}\") {{ id }} }}"),
            ),
            (
                PERM_LEAVE_MANAGE,
                "mutation { upsertLeaveType(input: { name: \"Annual\", code: \"AL\", isPaid: true, carryForward: false, sandwichRule: false, halfDayAllowed: true, requiresDocument: false }) { id } }".into(),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!("mutation {{ deleteLeaveType(leaveTypeId: \"{id}\") {{ id }} }}"),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!(
                    "mutation {{ upsertLeavePolicy(input: {{ leaveTypeId: \"{id}\" }}) {{ id }} }}"
                ),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!("mutation {{ deleteLeavePolicy(leavePolicyId: \"{id}\") }}"),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!(
                    "mutation {{ upsertLeaveBalance(input: {{ employeeId: \"{id}\", leaveTypeId: \"{id}\", year: 2026, entitledDays: \"12\", usedDays: \"0\", pendingDays: \"0\", carriedForwardDays: \"0\" }}) {{ id }} }}"
                ),
            ),
            (
                PERM_LEAVE_MANAGE,
                format!(
                    "mutation {{ adjustLeaveBalanceEntitlement(input: {{ employeeId: \"{id}\", leaveTypeId: \"{id}\", year: 2026, entitledDelta: \"1\" }}) {{ id }} }}"
                ),
            ),
            (
                PERM_LEAVE_MANAGE,
                "mutation { provisionLeaveBalancesFromPolicies(year: 2026) }".into(),
            ),
        ]
    }

    fn assert_exact_permission_denied_before_db(
        response: &async_graphql::Response,
        permission: &str,
    ) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(permission),
            "expected exact {permission} denial, got: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    fn assert_authorization_reached_db(
        response: &async_graphql::Response,
        permission: &str,
    ) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            !message.contains(permission),
            "valid exact authority was rejected: {message}"
        );
        let code = response.errors[0]
            .extensions
            .as_ref()
            .and_then(|extensions| extensions.get("code"))
            .cloned();
        assert_eq!(
            code,
            Some(async_graphql::Value::from("INTERNAL_ERROR")),
            "valid authority did not reach the tenant database boundary: {message}"
        );
    }

    #[tokio::test]
    async fn every_leave_mutation_denies_missing_and_sibling_permissions_before_db_access() {
        for (required_permission, mutation) in mutation_inventory() {
            let missing = execute_mutation(claims(None, None), &mutation).await;
            assert_exact_permission_denied_before_db(&missing, required_permission);

            let sibling = execute_mutation(
                claims(Some(PERM_LEAVE_READ), Some("ALL")),
                &mutation,
            )
            .await;
            assert_exact_permission_denied_before_db(&sibling, required_permission);
        }
    }

    #[tokio::test]
    async fn every_leave_mutation_rejects_missing_malformed_or_unsuitable_exact_scope() {
        for (required_permission, mutation) in mutation_inventory() {
            let unsuitable_scopes: &[Option<&str>] = match required_permission {
                PERM_LEAVE_SUBMIT => &[None, Some("INVALID"), Some("TEAM"), Some("ALL")],
                PERM_LEAVE_APPROVE => {
                    &[None, Some("INVALID"), Some("SELF"), Some("DEPARTMENT")]
                }
                PERM_LEAVE_MANAGE => &[
                    None,
                    Some("INVALID"),
                    Some("SELF"),
                    Some("TEAM"),
                    Some("DEPARTMENT"),
                ],
                _ => unreachable!("inventory contains only leave mutation authorities"),
            };

            for scope in unsuitable_scopes {
                let response =
                    execute_mutation(claims(Some(required_permission), *scope), &mutation).await;
                assert_exact_permission_denied_before_db(&response, required_permission);
            }
        }
    }

    #[tokio::test]
    async fn suitable_exact_scope_allows_each_mutation_to_reach_its_loader_boundary() {
        assert_eq!(mutation_inventory().len(), 11);
        for (required_permission, mutation) in mutation_inventory() {
            let scope = match required_permission {
                PERM_LEAVE_SUBMIT => "SELF",
                PERM_LEAVE_APPROVE => "TEAM",
                PERM_LEAVE_MANAGE => "ALL",
                _ => unreachable!("inventory contains only leave mutation authorities"),
            };
            let response =
                execute_mutation(claims(Some(required_permission), Some(scope)), &mutation).await;
            assert_authorization_reached_db(&response, required_permission);
        }
    }

    #[tokio::test]
    async fn approval_mutations_accept_only_the_configured_team_or_all_authority() {
        for (_, mutation) in mutation_inventory()
            .into_iter()
            .filter(|(permission, _)| *permission == PERM_LEAVE_APPROVE)
        {
            for scope in ["TEAM", "ALL"] {
                let response = execute_mutation(
                    claims(Some(PERM_LEAVE_APPROVE), Some(scope)),
                    &mutation,
                )
                .await;
                assert_authorization_reached_db(&response, PERM_LEAVE_APPROVE);
            }
        }
    }

    #[tokio::test]
    async fn approval_mutations_require_the_clients_expected_workflow_step() {
        let id = Uuid::new_v4();
        for mutation in [
            format!("mutation {{ approveLeaveRequest(leaveRequestId: \"{id}\") {{ id }} }}"),
            format!(
                "mutation {{ rejectLeaveRequest(leaveRequestId: \"{id}\", reason: \"invalid\") {{ id }} }}"
            ),
        ] {
            let response = execute_mutation(
                claims(Some(PERM_LEAVE_APPROVE), Some("TEAM")),
                &mutation,
            )
            .await;
            assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
            assert!(
                response.errors[0].message.contains("expectedWorkflowStepId"),
                "expected required expectedWorkflowStepId validation, got: {:?}",
                response.errors[0]
            );
        }
    }

    #[tokio::test]
    async fn balance_adjustment_rejects_the_removed_also_credit_balance_field() {
        let id = Uuid::new_v4();
        let response = execute_mutation(
            claims(Some(PERM_LEAVE_MANAGE), Some("ALL")),
            &format!(
                "mutation {{ adjustLeaveBalanceEntitlement(input: {{ employeeId: \"{id}\", leaveTypeId: \"{id}\", year: 2026, entitledDelta: \"1\", alsoCreditBalance: true }}) {{ id }} }}"
            ),
        )
        .await;
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        assert!(
            response.errors[0].message.contains("alsoCreditBalance"),
            "removed field must fail GraphQL validation: {:?}",
            response.errors[0]
        );
        assert_ne!(
            response.errors[0]
                .extensions
                .as_ref()
                .and_then(|extensions| extensions.get("code"))
                .cloned(),
            Some(async_graphql::Value::from("INTERNAL_ERROR")),
            "removed field must not reach the tenant database"
        );
    }

}
