//! Root query resolvers for kabipay-leave.

use async_graphql::{Context, Object, Result, ID};
use chrono::NaiveDate;
use kabipay_common::client_data_scope::{
    data_scope_from_claims, resolve_employee_scope_filter, resolve_viewer_employee,
    EmployeeScopeFilter,
};
use kabipay_common::context::{ClientClaims, ScopeType, PERM_LEAVE_READ};
use kabipay_common::{
    subgraph::{require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0011_leave::leave_request;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use std::collections::HashMap;
use uuid::Uuid;

use crate::resolvers::types::{
    LeaveBalanceDto, LeavePolicyDto, LeaveRequestDto, LeaveTypeDto, LeaveWorkflowActionDto,
};
use crate::services::{leave_admin, leave_service};

pub(crate) fn parse_uuid(raw: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(raw.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn leave_read_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    data_scope_from_claims(claims, PERM_LEAVE_READ)
}

fn leave_read_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    leave_read_scope_from_claims(ctx.data_opt::<ClientClaims>())
        .map_err(KabiPayError::into_graphql)
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn leave_health(&self) -> &'static str {
        "ok"
    }

    /// Canonical `EMPLOYEE.id` for the authenticated client (from JWT → user → employee link).
    async fn viewer_employee_id(&self, ctx: &Context<'_>) -> Result<ID> {
        let tenant_id = require_tenant_id(ctx)?;
        leave_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(ID(id.to_string()))
    }

    /// List leave types for the caller's tenant.
    async fn leave_types(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<LeaveTypeDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        leave_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = leave_service::list_types(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(LeaveTypeDto::from).collect())
    }

    /// List leave requests for the caller's tenant.
    async fn leave_requests(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
        from_date: Option<NaiveDate>,
        to_date: Option<NaiveDate>,
    ) -> Result<Vec<LeaveRequestDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = leave_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filter = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if let (Some(from), Some(to)) = (from_date, to_date) {
            if from > to {
                return Err(KabiPayError::Validation(
                    "fromDate must be on or before toDate".into(),
                )
                .into_graphql());
            }
        }
        let mut query = leave_request::Entity::find()
            .filter(leave_request::Column::TenantId.eq(tenant_id))
            .filter(leave_request::Column::IsDeleted.eq(false));
        if let Some(from) = from_date {
            query = query.filter(leave_request::Column::ToDate.gte(from));
        }
        if let Some(to) = to_date {
            query = query.filter(leave_request::Column::FromDate.lte(to));
        }
        match &filter {
            EmployeeScopeFilter::Unrestricted => {}
            EmployeeScopeFilter::Empty => return Ok(vec![]),
            EmployeeScopeFilter::EmployeeIds(ids) if ids.is_empty() => return Ok(vec![]),
            EmployeeScopeFilter::EmployeeIds(ids) => {
                query = query.filter(leave_request::Column::EmployeeId.is_in(ids.clone()));
            }
        }
        let rows = query
            .order_by_desc(leave_request::Column::AppliedAt)
            .limit(limit.clamp(1, 200))
            .all(&db)
            .await
            .map_err(|error| KabiPayError::from(error).into_graphql())?;
        let mut employee_ids: Vec<Uuid> = rows.iter().map(|row| row.employee_id).collect();
        employee_ids.sort_unstable();
        employee_ids.dedup();
        let employee_labels: HashMap<Uuid, (String, String)> = if employee_ids.is_empty() {
            HashMap::new()
        } else {
            employee::Entity::find()
                .filter(employee::Column::TenantId.eq(tenant_id))
                .filter(employee::Column::IsDeleted.eq(false))
                .filter(employee::Column::Id.is_in(employee_ids))
                .all(&db)
                .await
                .map_err(|error| KabiPayError::from(error).into_graphql())?
                .into_iter()
                .map(|employee| {
                    let name = format!("{} {}", employee.first_name, employee.last_name)
                        .trim()
                        .to_string();
                    (employee.id, (name, employee.employee_code))
                })
                .collect()
        };

        Ok(rows
            .into_iter()
            .map(|row| {
                let label = employee_labels.get(&row.employee_id).cloned();
                let dto = LeaveRequestDto::from(row);
                match label {
                    Some((name, code)) => dto.with_employee_label(name, code),
                    None => dto,
                }
            })
            .collect())
    }

    /// Leave-balance rows for an employee. Pass `employeeId` to target a
    /// specific person (e.g. HR view); when omitted, the caller's own
    /// employee id is resolved from the JWT (requires `Authorization`).
    async fn leave_balances(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        year: Option<i32>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<LeaveBalanceDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = leave_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = match &employee_id {
            Some(id) => parse_uuid(id, "employeeId")?,
            None => resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?,
        };
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filt = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if !filt.allows_employee(emp) {
            return Ok(vec![]);
        }
        let rows = leave_service::list_balances_for_employee(&db, tenant_id, emp, year, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(LeaveBalanceDto::from).collect())
    }

    /// Published leave policies for the tenant (configuration reference for employees and HR).
    async fn leave_policies(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<LeavePolicyDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        leave_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = leave_admin::list_leave_policies(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(LeavePolicyDto::from).collect())
    }

    /// Workflow step actions recorded for a leave request (empty when no workflow instance).
    async fn leave_request_workflow_trail(
        &self,
        ctx: &Context<'_>,
        leave_request_id: ID,
    ) -> Result<Vec<LeaveWorkflowActionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = leave_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let filter = resolve_employee_scope_filter(&db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let rid = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let req = leave_request::Entity::find_by_id(rid)
            .filter(leave_request::Column::TenantId.eq(tenant_id))
            .filter(leave_request::Column::IsDeleted.eq(false))
            .one(&db)
            .await
            .map_err(|error| KabiPayError::from(error).into_graphql())?;
        let Some(req) = req.filter(|request| filter.allows_employee(request.employee_id)) else {
            return Err(KabiPayError::Forbidden(
                "leave request not found or not visible".into(),
            )
            .into_graphql());
        };
        let Some(inst) = req.workflow_instance_id else {
            return Ok(vec![]);
        };
        let rows = leave_service::leave_workflow_action_trail(&db, tenant_id, inst)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|(a, step)| LeaveWorkflowActionDto::from_action(step, a))
            .collect())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Request, Schema};
    use kabipay_common::context::{ClientClaims, ScopeType, CLIENT_JWT_ISSUER, PERM_LEAVE_APPROVE};
    use kabipay_common::subgraph::TenantId;
    use serde_json::json;

    fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
        let permission_scopes = scope
            .map(|scope| HashMap::from([(permission.to_string(), scope.to_string())]))
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
            permissions: vec![permission.into()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    fn claims_without_permissions() -> ClientClaims {
        let mut claims = claims("unrelated:read", Some("ALL"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        claims
    }

    async fn execute_query(claims: ClientClaims, query: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(query))
            .await
    }

    fn assert_leave_read_denied_before_db(response: &async_graphql::Response) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains("leave:read permission required"),
            "unexpected denial: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    struct LeaveTargetScopeBoundaryQuery;

    #[Object]
    impl LeaveTargetScopeBoundaryQuery {
        async fn leave_target_visible(
            &self,
            ctx: &Context<'_>,
            target_employee_id: ID,
        ) -> Result<bool> {
            leave_read_scope(ctx)?;
            let filter = ctx.data::<EmployeeScopeFilter>()?;
            Ok(filter.allows_employee(parse_uuid(
                &target_employee_id,
                "targetEmployeeId",
            )?))
        }
    }

    async fn execute_target_scope_query(
        wire_scope: &str,
        filter: EmployeeScopeFilter,
        target_employee_id: Uuid,
    ) -> async_graphql::Response {
        Schema::build(
            LeaveTargetScopeBoundaryQuery,
            EmptyMutation,
            EmptySubscription,
        )
        .data(claims(PERM_LEAVE_READ, Some(wire_scope)))
        .data(filter)
        .finish()
        .execute(Request::new(format!(
            "{{ leaveTargetVisible(targetEmployeeId: \"{target_employee_id}\") }}"
        )))
        .await
    }

    #[test]
    fn leave_read_gate_accepts_self_recursive_team_and_all_scopes() {
        for (wire_scope, expected) in [
            ("SELF", ScopeType::Self_),
            ("TEAM", ScopeType::Team),
            ("ALL", ScopeType::All),
        ] {
            let claims = claims(PERM_LEAVE_READ, Some(wire_scope));

            let actual = leave_read_scope_from_claims(Some(&claims))
                .expect("valid exact leave read scope");
            assert_eq!(actual, expected);
        }
    }

    #[test]
    fn leave_read_gate_rejects_missing_permission_and_missing_or_malformed_exact_scope() {
        let approve_only = claims(PERM_LEAVE_APPROVE, Some("ALL"));
        assert!(matches!(
            leave_read_scope_from_claims(Some(&approve_only)),
            Err(KabiPayError::Forbidden(_))
        ));

        for scope in [None, Some("INVALID")] {
            let claims = claims(PERM_LEAVE_READ, scope);
            assert!(matches!(
                leave_read_scope_from_claims(Some(&claims)),
                Err(KabiPayError::Forbidden(_))
            ));
        }

        assert!(matches!(
            leave_read_scope_from_claims(None),
            Err(KabiPayError::Unauthorised)
        ));
    }

    #[tokio::test]
    async fn every_protected_leave_query_denies_missing_and_sibling_permissions_before_db_access() {
        let leave_request_id = Uuid::new_v4();
        let fields = vec![
            "{ viewerEmployeeId }".to_string(),
            "{ leaveTypes { __typename } }".to_string(),
            "{ leaveRequests { __typename } }".to_string(),
            "{ leaveBalances { __typename } }".to_string(),
            "{ leavePolicies { __typename } }".to_string(),
            format!(
                "{{ leaveRequestWorkflowTrail(leaveRequestId: \"{leave_request_id}\") {{ __typename }} }}"
            ),
        ];

        for query in fields {
            let missing = execute_query(claims_without_permissions(), &query).await;
            assert_leave_read_denied_before_db(&missing);

            let sibling =
                execute_query(claims(PERM_LEAVE_APPROVE, Some("ALL")), &query).await;
            assert_leave_read_denied_before_db(&sibling);
        }
    }

    #[tokio::test]
    async fn leave_target_filter_enforces_self_recursive_team_and_all() {
        let viewer_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();

        for (wire_scope, filter, target_employee_id, expected) in [
            ("SELF", EmployeeScopeFilter::EmployeeIds(vec![viewer_id]), viewer_id, true),
            ("SELF", EmployeeScopeFilter::EmployeeIds(vec![viewer_id]), descendant_id, false),
            ("TEAM", EmployeeScopeFilter::EmployeeIds(vec![viewer_id, descendant_id]), descendant_id, true),
            ("TEAM", EmployeeScopeFilter::EmployeeIds(vec![viewer_id, descendant_id]), outside_id, false),
            ("ALL", EmployeeScopeFilter::Unrestricted, outside_id, true),
        ] {
            let response =
                execute_target_scope_query(wire_scope, filter, target_employee_id).await;
            assert!(response.errors.is_empty(), "unexpected response: {response:?}");
            assert_eq!(
                response.data.into_json().expect("valid response data"),
                json!({"leaveTargetVisible": expected})
            );
        }
    }
}
