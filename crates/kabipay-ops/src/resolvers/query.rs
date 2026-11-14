//! Root query resolvers for kabipay-ops.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::subgraph::{ops_db, require_operator_context};
use kabipay_common::context::OperatorContext;
use kabipay_common::KabiPayError;
use uuid::Uuid;

use crate::resolvers::types::{
    BillingCycleDto, FeatureFlagDto, InvoiceDto, ModuleDto, OperatorRoleDto, OperatorUserDto,
    PaymentDto, TenantDto, TenantSubscriptionDto,
};
use crate::services::{billing_service, operator_service, tenant_service, tenant_write_service};

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn operator_health(&self) -> &'static str {
        "ok"
    }

    async fn tenant_health(&self) -> &'static str {
        "ok"
    }

    async fn billing_health(&self) -> &'static str {
        "ok"
    }

    async fn tenants(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TenantDto>> {
        let operator = require_operator_context(ctx)?;
        let allowed = authorized_tenant_ids(operator, None)?;
        let db = ops_db(ctx)?;
        let rows = tenant_service::list_tenants(db, allowed.as_deref(), limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantDto::from).collect())
    }

    async fn modules(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        #[graphql(default = false)] include_inactive: bool,
    ) -> Result<Vec<ModuleDto>> {
        if include_inactive {
            require_operator_context(ctx)?;
        }
        let db = ops_db(ctx)?;
        let rows = tenant_service::list_modules(db, limit, include_inactive)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ModuleDto::from).collect())
    }

    async fn tenant_subscriptions(
        &self,
        ctx: &Context<'_>,
        tenant_id: Option<ID>,
        #[graphql(default = 200)] limit: u64,
    ) -> Result<Vec<TenantSubscriptionDto>> {
        let operator = require_operator_context(ctx)?;
        let tenant = parse_uuid_opt(tenant_id, "tenantId")?;
        let allowed = authorized_tenant_ids(operator, tenant)?;
        let db = ops_db(ctx)?;
        let rows = tenant_service::list_subscriptions(db, allowed.as_deref(), limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(TenantSubscriptionDto::from)
            .collect())
    }

    async fn feature_flags(
        &self,
        ctx: &Context<'_>,
        tenant_id: ID,
        #[graphql(default = 200)] limit: u64,
    ) -> Result<Vec<FeatureFlagDto>> {
        let operator = require_operator_context(ctx)?;
        let tid = Uuid::parse_str(&tenant_id.0).map_err(|e| {
            KabiPayError::Validation(format!("tenantId is not a UUID: {e}")).into_graphql()
        })?;
        authorized_tenant_ids(operator, Some(tid))?;
        let db = ops_db(ctx)?;
        let rows = tenant_write_service::list_feature_flags(db, tid, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(FeatureFlagDto::from).collect())
    }

    async fn operator_users(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<OperatorUserDto>> {
        require_global_operator(ctx)?;
        let db = ops_db(ctx)?;
        let rows = operator_service::list_users(db, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(OperatorUserDto::from).collect())
    }

    async fn operator_roles(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<OperatorRoleDto>> {
        require_global_operator(ctx)?;
        let db = ops_db(ctx)?;
        let rows = operator_service::list_roles(db, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(OperatorRoleDto::from).collect())
    }

    /// Roles assigned to one operator user (junction `operator_user_role`).
    async fn operator_roles_for_user(
        &self,
        ctx: &Context<'_>,
        operator_user_id: ID,
    ) -> Result<Vec<OperatorRoleDto>> {
        require_global_operator(ctx)?;
        let db = ops_db(ctx)?;
        let uid = Uuid::parse_str(&operator_user_id.0).map_err(|e| {
            KabiPayError::Validation(format!("operatorUserId is not a UUID: {e}")).into_graphql()
        })?;
        let rows = operator_service::roles_for_user(db, uid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(OperatorRoleDto::from).collect())
    }

    async fn invoices(
        &self,
        ctx: &Context<'_>,
        tenant_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<InvoiceDto>> {
        let operator = require_operator_context(ctx)?;
        let tenant = parse_uuid_opt(tenant_id, "tenantId")?;
        let allowed = authorized_tenant_ids(operator, tenant)?;
        let db = ops_db(ctx)?;
        let rows = billing_service::list_invoices(db, allowed.as_deref(), limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(InvoiceDto::from).collect())
    }

    async fn payments(
        &self,
        ctx: &Context<'_>,
        invoice_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<PaymentDto>> {
        let operator = require_operator_context(ctx)?;
        let allowed = authorized_tenant_ids(operator, None)?;
        let db = ops_db(ctx)?;
        let inv = parse_uuid_opt(invoice_id, "invoiceId")?;
        let rows = billing_service::list_payments(db, inv, allowed.as_deref(), limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(PaymentDto::from).collect())
    }

    async fn billing_cycles(
        &self,
        ctx: &Context<'_>,
        tenant_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<BillingCycleDto>> {
        let operator = require_operator_context(ctx)?;
        let tenant = parse_uuid_opt(tenant_id, "tenantId")?;
        let allowed = authorized_tenant_ids(operator, tenant)?;
        let db = ops_db(ctx)?;
        let rows = billing_service::list_billing_cycles(db, allowed.as_deref(), limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(BillingCycleDto::from).collect())
    }
}

/// Resolve the authorized population before SQL pagination, never by filtering DTOs.
fn authorized_tenant_ids(operator: &OperatorContext, requested: Option<Uuid>) -> Result<Option<Vec<Uuid>>> {
    if let Some(tenant) = requested {
        if !operator.can_access_tenant(tenant) {
            return Err(KabiPayError::Forbidden("operator has no access to this tenant".into()).into_graphql());
        }
        return Ok(Some(vec![tenant]));
    }
    Ok(if operator.is_super_admin() { None } else { Some(operator.tenant_access.clone()) })
}

fn require_global_operator<'a>(ctx: &'a Context<'a>) -> Result<&'a OperatorContext> {
    let operator = require_operator_context(ctx)?;
    if !operator.is_super_admin() {
        return Err(KabiPayError::Forbidden("global operator authority required".into()).into_graphql());
    }
    Ok(operator)
}

fn parse_uuid_opt(id: Option<ID>, field: &str) -> Result<Option<Uuid>> {
    match id {
        None => Ok(None),
        Some(id) => Uuid::parse_str(&id.0).map(Some).map_err(|e| {
            KabiPayError::Validation(format!("{field} is not a UUID: {e}")).into_graphql()
        }),
    }
}

#[cfg(test)]
#[path = "authorization_tests.rs"]
mod authorization_tests;
