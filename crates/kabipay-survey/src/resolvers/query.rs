use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    context::ScopeType,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

use super::types::{SurveyDto, SurveyResultsDto, SurveySummaryDto};
use crate::services::survey_service::{self, ResultScope};

fn parse_id(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| KabiPayError::Validation("Invalid ID".into()).into_graphql())
}

fn employee_id(claims: &kabipay_common::context::ClientClaims) -> Result<Uuid> {
    claims.employee_id.ok_or_else(|| KabiPayError::Forbidden("An employee-linked account is required".into()).into_graphql())
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn survey_audience(&self, ctx: &Context<'_>, survey_id: ID) -> Result<crate::services::survey_management::SurveyAudience> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() { return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql()); }
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        crate::services::survey_management::audience(&db, tenant_id, survey_id).await.map_err(KabiPayError::into_graphql)
    }

    async fn survey_audience_options(&self, ctx: &Context<'_>, kind: String, search: Option<String>, after: Option<ID>, #[graphql(default = 50)] limit: i32) -> Result<crate::services::survey_management::SurveyAudienceOptions> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() { return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql()); }
        let tenant_id = require_tenant_id(ctx)?;
        let after = after.as_ref().map(parse_id).transpose()?;
        let db = tenant_db(ctx, tenant_id).await?;
        crate::services::survey_management::options(&db, tenant_id, &kind, search, after, limit).await.map_err(KabiPayError::into_graphql)
    }

    async fn survey_health(&self) -> &'static str { "ok" }

    async fn survey_management_events(&self, ctx: &Context<'_>, survey_id: ID) -> Result<Vec<super::types::SurveyManagementEventDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() {
            return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        crate::services::survey_lifecycle::load_management_events(&db, tenant_id, survey_id)
            .await.map_err(KabiPayError::into_graphql)
    }

    async fn surveys(&self, ctx: &Context<'_>) -> Result<Vec<SurveySummaryDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() {
            return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        survey_service::list_surveys(&db, tenant_id).await.map_err(KabiPayError::into_graphql)
    }

    async fn available_surveys(&self, ctx: &Context<'_>) -> Result<Vec<SurveySummaryDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_respond_to_surveys() {
            return Err(KabiPayError::Forbidden("survey:respond with SELF scope required".into()).into_graphql());
        }
        let employee_id = employee_id(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        survey_service::list_available_surveys(&db, tenant_id, employee_id).await.map_err(KabiPayError::into_graphql)
    }

    async fn survey_results_catalog(&self, ctx: &Context<'_>) -> Result<Vec<SurveySummaryDto>> {
        let claims = require_client_claims(ctx)?;
        if claims.survey_results_scope().is_none() {
            return Err(KabiPayError::Forbidden("survey:results with TEAM, DEPARTMENT, or ALL scope required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        survey_service::list_results_surveys(&db, tenant_id).await.map_err(KabiPayError::into_graphql)
    }

    async fn survey(&self, ctx: &Context<'_>, survey_id: ID) -> Result<SurveyDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() && !claims.can_respond_to_surveys() {
            return Err(KabiPayError::Forbidden("Survey access is not permitted".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let completed = survey_service::completion_for_viewer(
            &db,
            tenant_id,
            survey_id,
            claims.employee_id,
            claims.can_manage_surveys(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        survey_service::load_survey(&db, tenant_id, survey_id, completed).await.map_err(KabiPayError::into_graphql)
    }

    async fn survey_results(&self, ctx: &Context<'_>, survey_id: ID) -> Result<SurveyResultsDto> {
        let claims = require_client_claims(ctx)?;
        let result_scope = claims.survey_results_scope().ok_or_else(|| {
            KabiPayError::Forbidden("survey:results with TEAM, DEPARTMENT, or ALL scope required".into()).into_graphql()
        })?;
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let scope = match result_scope {
            ScopeType::All => ResultScope::All,
            ScopeType::Team => ResultScope::Team(employee_id(claims)?),
            ScopeType::Department => {
                let employee_id = employee_id(claims)?;
                let department_id = employee::Entity::find_by_id(employee_id)
                    .filter(employee::Column::TenantId.eq(tenant_id))
                    .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
                    .and_then(|employee| employee.department_id)
                    .ok_or_else(|| KabiPayError::Forbidden("A department-linked employee account is required".into()).into_graphql())?;
                ResultScope::Department(department_id)
            }
            ScopeType::Self_ => return Err(KabiPayError::Forbidden("SELF scope cannot access aggregate survey results".into()).into_graphql()),
        };
        survey_service::aggregate_results(&db, tenant_id, survey_id, scope).await.map_err(KabiPayError::into_graphql)
    }
}
