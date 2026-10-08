//! GraphQL DTOs for kabipay-analytics.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use sea_orm::prelude::Json;

use crate::entities::d0024_analytics::{
    dashboard, dashboard_widget, report_definition, report_schedule, workforce_snapshot,
};
use crate::entities::d0030_outbox_events::outbox_event;

fn opt_json_str(v: &Option<Json>) -> Option<String> {
    v.as_ref().and_then(|j| serde_json::to_string(j).ok())
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ReportDefinitionRow")]
pub struct ReportDefinitionDto {
    pub id: ID,
    pub name: String,
    pub entity_type: Option<String>,
    pub filters_json: Option<String>,
    pub columns_json: Option<String>,
    pub groupby_json: Option<String>,
    pub chart_type: Option<String>,
    pub is_public: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<report_definition::Model> for ReportDefinitionDto {
    fn from(m: report_definition::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            name: m.name,
            entity_type: m.entity_type,
            filters_json: opt_json_str(&m.filters_json),
            columns_json: opt_json_str(&m.columns_json),
            groupby_json: opt_json_str(&m.groupby_json),
            chart_type: m.chart_type,
            is_public: m.is_public,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ReportScheduleRow")]
pub struct ReportScheduleDto {
    pub id: ID,
    pub report_definition_id: ID,
    pub frequency: String,
    pub recipients_json: Option<String>,
    pub delivery_format: Option<String>,
    pub is_active: bool,
    pub last_sent_at: Option<DateTime<Utc>>,
    pub next_run_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<report_schedule::Model> for ReportScheduleDto {
    fn from(m: report_schedule::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            report_definition_id: ID(m.report_definition_id.to_string()),
            frequency: m.frequency,
            recipients_json: opt_json_str(&m.recipients_json),
            delivery_format: m.delivery_format,
            is_active: m.is_active,
            last_sent_at: m.last_sent_at,
            next_run_at: m.next_run_at,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "DashboardRow")]
pub struct DashboardRowDto {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub is_default: bool,
    pub created_at: DateTime<Utc>,
}

impl From<dashboard::Model> for DashboardRowDto {
    fn from(m: dashboard::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            name: m.name,
            description: m.description,
            is_default: m.is_default,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "DashboardWidgetRow")]
pub struct DashboardWidgetRowDto {
    pub id: ID,
    pub dashboard_id: ID,
    pub report_definition_id: Option<ID>,
    pub widget_type: Option<String>,
    pub title: Option<String>,
    pub grid_col: Option<i32>,
    pub grid_row: Option<i32>,
    pub col_span: Option<i32>,
    pub row_span: Option<i32>,
}

impl From<dashboard_widget::Model> for DashboardWidgetRowDto {
    fn from(m: dashboard_widget::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            dashboard_id: ID(m.dashboard_id.to_string()),
            report_definition_id: m.report_definition_id.map(|u| ID(u.to_string())),
            widget_type: m.widget_type,
            title: m.title,
            grid_col: m.grid_col,
            grid_row: m.grid_row,
            col_span: m.col_span,
            row_span: m.row_span,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "WorkforceSnapshotRow")]
pub struct WorkforceSnapshotDto {
    pub id: ID,
    pub snapshot_date: NaiveDate,
    pub total_headcount: Option<i32>,
    pub active_employees: Option<i32>,
    pub new_joiners: Option<i32>,
    pub separations: Option<i32>,
    pub open_positions: Option<i32>,
    pub average_tenure_months: Option<String>,
    pub attrition_rate: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<workforce_snapshot::Model> for WorkforceSnapshotDto {
    fn from(m: workforce_snapshot::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            snapshot_date: m.snapshot_date,
            total_headcount: m.total_headcount,
            active_employees: m.active_employees,
            new_joiners: m.new_joiners,
            separations: m.separations,
            open_positions: m.open_positions,
            average_tenure_months: m.average_tenure_months.map(|d| d.to_string()),
            attrition_rate: m.attrition_rate.map(|d| d.to_string()),
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "OutboxEventRow")]
pub struct OutboxEventDto {
    pub id: ID,
    pub aggregate_type: String,
    pub aggregate_id: ID,
    pub event_type: String,
    pub payload_json: String,
    pub status: String,
    pub retry_count: i32,
    pub last_error: Option<String>,
    pub created_at: DateTime<Utc>,
    pub processed_at: Option<DateTime<Utc>>,
    /// Present while the outbox worker holds the row (`PROCESSING`).
    pub claimed_at: Option<DateTime<Utc>>,
}

impl From<outbox_event::Model> for OutboxEventDto {
    fn from(m: outbox_event::Model) -> Self {
        let payload_json = serde_json::to_string(&m.payload).unwrap_or_else(|_| "{}".to_string());
        Self {
            id: ID(m.id.to_string()),
            aggregate_type: m.aggregate_type,
            aggregate_id: ID(m.aggregate_id.to_string()),
            event_type: m.event_type,
            payload_json,
            status: m.status,
            retry_count: m.retry_count,
            last_error: m.last_error,
            created_at: m.created_at,
            processed_at: m.processed_at,
            claimed_at: m.claimed_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "IntegrationConnectorCatalogRow")]
pub struct IntegrationConnectorCatalogDto {
    pub id: ID,
    pub name: String,
    pub code: String,
    pub category: Option<String>,
    pub auth_type: Option<String>,
    pub is_active: bool,
}

impl From<kabipay_db_entities::ops::integration_connector::Model>
    for IntegrationConnectorCatalogDto
{
    fn from(m: kabipay_db_entities::ops::integration_connector::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            name: m.name,
            code: m.code,
            category: m.category,
            auth_type: m.auth_type,
            is_active: m.is_active,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "TenantIntegrationRow")]
pub struct TenantIntegrationDto {
    pub id: ID,
    pub integration_connector_id: ID,
    pub is_active: bool,
    pub connected_at: Option<DateTime<Utc>>,
    pub created_at: DateTime<Utc>,
}

impl From<kabipay_db_entities::tenant::d0026_integrations::tenant_integration::Model>
    for TenantIntegrationDto
{
    fn from(m: kabipay_db_entities::tenant::d0026_integrations::tenant_integration::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            integration_connector_id: ID(m.integration_connector_id.to_string()),
            is_active: m.is_active,
            connected_at: m.connected_at,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "WebhookSubscriptionRow")]
pub struct WebhookSubscriptionDto {
    pub id: ID,
    pub event_name: String,
    pub endpoint_url: String,
    /// Stored as SHA256 hex (`None` when no signing secret configured).
    pub secret_hash: Option<String>,
    pub is_active: bool,
    pub created_at: DateTime<Utc>,
}

impl From<kabipay_db_entities::tenant::d0026_integrations::webhook_subscription::Model>
    for WebhookSubscriptionDto
{
    fn from(
        m: kabipay_db_entities::tenant::d0026_integrations::webhook_subscription::Model,
    ) -> Self {
        Self {
            id: ID(m.id.to_string()),
            event_name: m.event_name,
            endpoint_url: m.endpoint_url,
            secret_hash: m.secret_hash,
            is_active: m.is_active,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "WebhookDeliveryLogRow")]
pub struct WebhookDeliveryLogDto {
    pub id: ID,
    pub webhook_subscription_id: ID,
    pub event_name: Option<String>,
    pub payload_json: Option<String>,
    pub http_status: Option<i32>,
    pub response_body: Option<String>,
    pub is_success: bool,
    pub attempt_number: i32,
    pub delivered_at: DateTime<Utc>,
    pub created_at: DateTime<Utc>,
}

impl From<kabipay_db_entities::tenant::d0026_integrations::webhook_delivery_log::Model>
    for WebhookDeliveryLogDto
{
    fn from(
        m: kabipay_db_entities::tenant::d0026_integrations::webhook_delivery_log::Model,
    ) -> Self {
        Self {
            id: ID(m.id.to_string()),
            webhook_subscription_id: ID(m.webhook_subscription_id.to_string()),
            event_name: m.event_name,
            payload_json: opt_json_str(&m.payload_json),
            http_status: m.http_status,
            response_body: m.response_body,
            is_success: m.is_success,
            attempt_number: m.attempt_number,
            delivered_at: m.delivered_at,
            created_at: m.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AuditLogRow")]
pub struct AuditLogDto {
    pub id: ID,
    pub user_id: Option<ID>,
    pub entity_type: String,
    pub entity_id: Option<ID>,
    pub action: String,
    pub before_json: Option<String>,
    pub after_json: Option<String>,
    pub ip_address: Option<String>,
    pub created_at: DateTime<Utc>,
}

impl From<kabipay_db_entities::tenant::d0027_communication_audit::audit_log::Model>
    for AuditLogDto
{
    fn from(m: kabipay_db_entities::tenant::d0027_communication_audit::audit_log::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            user_id: m.user_id.map(|u| ID(u.to_string())),
            entity_type: m.entity_type,
            entity_id: m.entity_id.map(|u| ID(u.to_string())),
            action: m.action,
            before_json: m.before_state.and_then(|j| serde_json::to_string(&j).ok()),
            after_json: m.after_state.and_then(|j| serde_json::to_string(&j).ok()),
            ip_address: m.ip_address,
            created_at: m.created_at,
        }
    }
}

#[derive(InputObject)]
pub struct RegisterWebhookInput {
    pub event_name: String,
    pub endpoint_url: String,
    /// Optional signing secret (**SHA256** stored server-side).
    pub webhook_secret: Option<String>,
}
