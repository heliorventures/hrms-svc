//! Root query resolvers for kabipay-analytics.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{ops_db, require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AuditLogDto, DashboardRowDto, DashboardWidgetRowDto, IntegrationConnectorCatalogDto,
    OutboxEventDto, ReportDefinitionDto, ReportScheduleDto, TenantIntegrationDto,
    WebhookDeliveryLogDto, WebhookSubscriptionDto, WorkforceSnapshotDto,
};
use crate::services::analytics_service;

pub struct QueryRoot;

fn parse_opt_uuid(id: &Option<ID>, field: &'static str) -> Result<Option<Uuid>> {
    match id {
        None => Ok(None),
        Some(v) => Uuid::parse_str(v.as_str())
            .map_err(|e| {
                KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql()
            })
            .map(Some),
    }
}

fn require_analytics_insights(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_access_analytics_insights() {
        return Err(
            KabiPayError::Forbidden("analytics:read permission required".into()).into_graphql(),
        );
    }
    Ok(())
}

#[Object]
impl QueryRoot {
    async fn hr_report_rows(
        &self,
        ctx: &Context<'_>,
        kind: super::hr_report_types::HrReportKind,
        from_date: chrono::NaiveDate,
        to_date: chrono::NaiveDate,
        employee_id: Option<Uuid>,
        employee_search: Option<String>,
        #[graphql(default = 0)] offset: i32,
        #[graphql(default = 50)] limit: i32,
    ) -> Result<super::hr_report_types::HrReportRows> {
        let claims = require_client_claims(ctx)?;
        crate::services::hr_reports::authorize(claims, kind)
            .map_err(KabiPayError::into_graphql)?;
        let filter = crate::services::hr_reports::ReportFilter {
            from_date, to_date, employee_id, employee_search,
        };
        filter.validate().map_err(KabiPayError::into_graphql)?;
        if offset < 0 || !(1..=100).contains(&limit) {
            return Err(KabiPayError::Validation(
                "offset must be non-negative and limit between 1 and 100".into(),
            ).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        if tenant_id != claims.tenant_id {
            return Err(KabiPayError::Forbidden(
                "report tenant does not match authenticated tenant".into(),
            ).into_graphql());
        }
        let clock = kabipay_common::tenant_business_clock::TenantBusinessClock::load(
            ops_db(ctx)?, tenant_id,
        ).await.map_err(KabiPayError::into_graphql)?;
        let db = tenant_db(ctx, tenant_id).await?;
        crate::services::hr_reports::load(&db, tenant_id, claims, kind, &filter, clock)
            .await
            .and_then(|data| data.preview(offset, limit))
            .map_err(KabiPayError::into_graphql)
    }

    async fn hr_report_csv(
        &self,
        ctx: &Context<'_>,
        kind: super::hr_report_types::HrReportKind,
        from_date: chrono::NaiveDate,
        to_date: chrono::NaiveDate,
        employee_id: Option<Uuid>,
        employee_search: Option<String>,
    ) -> Result<super::hr_report_types::HrReportCsv> {
        let claims = require_client_claims(ctx)?;
        crate::services::hr_reports::authorize(claims, kind)
            .map_err(KabiPayError::into_graphql)?;
        let filter = crate::services::hr_reports::ReportFilter {
            from_date, to_date, employee_id, employee_search,
        };
        filter.validate().map_err(KabiPayError::into_graphql)?;
        let tenant_id = require_tenant_id(ctx)?;
        if tenant_id != claims.tenant_id {
            return Err(KabiPayError::Forbidden(
                "report tenant does not match authenticated tenant".into(),
            ).into_graphql());
        }
        let clock = kabipay_common::tenant_business_clock::TenantBusinessClock::load(
            ops_db(ctx)?, tenant_id,
        ).await.map_err(KabiPayError::into_graphql)?;
        let db = tenant_db(ctx, tenant_id).await?;
        crate::services::hr_reports::load(&db, tenant_id, claims, kind, &filter, clock)
            .await
            .and_then(|data| data.csv(kind, &filter))
            .map_err(KabiPayError::into_graphql)
    }

    async fn hr_insights(
        &self,
        ctx: &Context<'_>,
        from_date: chrono::NaiveDate,
        to_date: chrono::NaiveDate,
    ) -> Result<super::hr_report_types::HrInsights> {
        let claims = require_client_claims(ctx)?;
        if !crate::services::hr_reports::has_all(claims, "analytics:read") {
            return Err(KabiPayError::Forbidden(
                "analytics:read permission requires ALL scope".into(),
            ).into_graphql());
        }
        let filter = crate::services::hr_reports::ReportFilter {
            from_date, to_date, employee_id: None, employee_search: None,
        };
        filter.validate().map_err(KabiPayError::into_graphql)?;
        if ![
            "attendance:read", "employee:read", "payroll:read", "leave:read",
            "timesheet:read", "expense:read", "travel:read",
        ].iter().any(|permission| crate::services::hr_reports::has_all(claims, permission)) {
            return Ok(super::hr_report_types::HrInsights::default());
        }
        let tenant_id = require_tenant_id(ctx)?;
        if tenant_id != claims.tenant_id {
            return Err(KabiPayError::Forbidden(
                "report tenant does not match authenticated tenant".into(),
            ).into_graphql());
        }
        let clock = kabipay_common::tenant_business_clock::TenantBusinessClock::load(
            ops_db(ctx)?, tenant_id,
        ).await.map_err(KabiPayError::into_graphql)?;
        let db = tenant_db(ctx, tenant_id).await?;
        crate::services::hr_insights::load(&db, tenant_id, claims, &filter, clock)
            .await.map_err(KabiPayError::into_graphql)
    }

    async fn analytics_health(&self) -> &'static str {
        "ok"
    }

    async fn report_definitions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ReportDefinitionDto>> {
        require_analytics_insights(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_report_definitions(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ReportDefinitionDto::from).collect())
    }

    async fn report_schedules(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ReportScheduleDto>> {
        require_analytics_insights(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_report_schedules(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ReportScheduleDto::from).collect())
    }

    async fn dashboards(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<DashboardRowDto>> {
        require_analytics_insights(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_dashboards(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(DashboardRowDto::from).collect())
    }

    async fn dashboard_widgets(
        &self,
        ctx: &Context<'_>,
        dashboard_id: Option<ID>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<DashboardWidgetRowDto>> {
        require_analytics_insights(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let did = parse_opt_uuid(&dashboard_id, "dashboardId")?;
        let rows = analytics_service::list_dashboard_widgets(&db, tenant_id, did, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(DashboardWidgetRowDto::from).collect())
    }

    async fn workforce_snapshots(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 24)] limit: u64,
    ) -> Result<Vec<WorkforceSnapshotDto>> {
        require_analytics_insights(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_workforce_snapshots(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(WorkforceSnapshotDto::from).collect())
    }

    /// **HR / directory admins only** — inspect transactional outbox rows (e.g. after leave approval).
    async fn outbox_events(
        &self,
        ctx: &Context<'_>,
        status: Option<String>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<OutboxEventDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(
                KabiPayError::Forbidden(
                    "HR or employee directory access required to view outbox".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_outbox_events(&db, tenant_id, status, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(OutboxEventDto::from).collect())
    }

    /// **HR / directory admins only** — global connector catalogue (ops DB).
    async fn integration_connectors(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<IntegrationConnectorCatalogDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(
                KabiPayError::Forbidden(
                    "HR or employee directory access required to view integration connectors".into(),
                )
                .into_graphql(),
            );
        }
        let _ = require_tenant_id(ctx)?;
        let db = ops_db(ctx)?;
        let rows = analytics_service::list_integration_connectors_global(db, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(IntegrationConnectorCatalogDto::from).collect())
    }

    /// **HR / directory admins only** — tenant integration rows.
    async fn tenant_integrations(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TenantIntegrationDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(
                KabiPayError::Forbidden(
                    "HR or employee directory access required to view tenant integrations".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_tenant_integrations(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TenantIntegrationDto::from).collect())
    }

    /// **HR / directory admins only** — outbound webhook subscriptions.
    async fn webhook_subscriptions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<WebhookSubscriptionDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(
                KabiPayError::Forbidden(
                    "HR or employee directory access required to view webhook subscriptions".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_webhook_subscriptions(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(WebhookSubscriptionDto::from).collect())
    }

    /// **HR / directory admins only** — webhook POST attempts (**`webhook_delivery_log`**), newest first.
    async fn webhook_delivery_logs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<WebhookDeliveryLogDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(
                KabiPayError::Forbidden(
                    "HR or employee directory access required to view webhook delivery logs".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_webhook_delivery_logs(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(WebhookDeliveryLogDto::from).collect())
    }

    /// **HR / directory admins only** — communication/entity audit log (most recent first).
    async fn audit_logs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 200)] limit: u64,
    ) -> Result<Vec<AuditLogDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_employee_directory() {
            return Err(
                KabiPayError::Forbidden(
                    "HR or employee directory access required to view audit logs".into(),
                )
                .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = analytics_service::list_audit_logs(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AuditLogDto::from).collect())
    }
}
