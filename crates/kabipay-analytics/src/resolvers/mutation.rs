//! Mutations for kabipay-analytics (HR / directory tools).

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    OutboxEventDto, RegisterWebhookInput, TenantIntegrationDto, WebhookSubscriptionDto,
};
use crate::services::analytics_service;

pub struct MutationRoot;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

#[Object]
impl MutationRoot {
    /// Send a **FAILED** or **PROCESSING** outbox row back to **PENDING** (same RBAC as `outboxEvents`).
    async fn requeue_outbox_event(&self, ctx: &Context<'_>, id: ID) -> Result<OutboxEventDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "HR or directory access required to requeue outbox".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let oid = parse_uuid(&id, "id")?;
        let m = analytics_service::requeue_outbox_event(&db, tenant_id, oid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(OutboxEventDto::from(m))
    }

    /// Connect (or reconnect) an integration connector for this tenant (**HR / directory admins**).
    async fn connect_tenant_integration(
        &self,
        ctx: &Context<'_>,
        connector_id: ID,
    ) -> Result<TenantIntegrationDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "HR or employee directory access required to connect integrations".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cid = parse_uuid(&connector_id, "connectorId")?;
        let m = analytics_service::connect_tenant_integration(&db, tenant_id, cid)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(TenantIntegrationDto::from(m))
    }

    /// Register a webhook subscription (**HR / directory admins**).
    async fn register_webhook_subscription(
        &self,
        ctx: &Context<'_>,
        input: RegisterWebhookInput,
    ) -> Result<WebhookSubscriptionDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "HR or employee directory access required for webhook registration".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let m = analytics_service::register_webhook_subscription(
            &db,
            tenant_id,
            input.event_name,
            input.endpoint_url,
            input.webhook_secret,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(WebhookSubscriptionDto::from(m))
    }

    async fn set_webhook_subscription_active(
        &self,
        ctx: &Context<'_>,
        id: ID,
        active: bool,
    ) -> Result<WebhookSubscriptionDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(KabiPayError::Forbidden(
                "HR or employee directory access required to change webhooks".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let wid = parse_uuid(&id, "id")?;
        let m = analytics_service::set_webhook_subscription_active(&db, tenant_id, wid, active)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(WebhookSubscriptionDto::from(m))
    }
}
