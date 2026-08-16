//! Root query resolvers for kabipay-leave.

use async_graphql::{Context, Object, Result, ID};
use chrono::NaiveDate;
use kabipay_common::client_data_scope::{
    data_scope_from_context, resolve_employee_scope_filter, resolve_viewer_employee,
};
use kabipay_common::context::SCOPE_RES_LEAVE;
use kabipay_common::{
    subgraph::{require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
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

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn leave_health(&self) -> &'static str {
        "ok"
    }

    /// Canonical `EMPLOYEE.id` for the authenticated client (from JWT → user → employee link).
    async fn viewer_employee_id(&self, ctx: &Context<'_>) -> Result<ID> {
        let tenant_id = require_tenant_id(ctx)?;
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
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, SCOPE_RES_LEAVE);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let rows = leave_service::list_requests(
            &db,
            tenant_id,
            limit,
            scope,
            viewer,
            from_date,
            to_date,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
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
        let db = tenant_db(ctx, tenant_id).await?;
        let emp = match &employee_id {
            Some(id) => parse_uuid(id, "employeeId")?,
            None => resolve_client_employee_id(ctx, &db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)?,
        };
        let scope = data_scope_from_context(ctx, SCOPE_RES_LEAVE);
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
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = data_scope_from_context(ctx, SCOPE_RES_LEAVE);
        let viewer = resolve_viewer_employee(ctx, &db, tenant_id).await?;
        let rid = parse_uuid(&leave_request_id, "leaveRequestId")?;
        let req = leave_service::load_leave_request_for_viewer(&db, tenant_id, rid, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let Some(req) = req else {
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
