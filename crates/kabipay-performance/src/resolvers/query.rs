//! Root query resolvers for kabipay-performance.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{
    AppraisalTemplateDto, GoalDto, PerformanceProgramDto, PerformanceReviewDetailDto,
    PerformanceReviewSummaryDto, ReviewCycleDto,
};
use crate::services::{performance_lifecycle, performance_service, performance_workflow};

fn parse_id(id: &ID) -> Result<uuid::Uuid> {
    uuid::Uuid::parse_str(id.as_str())
        .map_err(|_| KabiPayError::Validation("Invalid ID".into()).into_graphql())
}

fn require_employee_id(claims: &kabipay_common::context::ClientClaims) -> Result<uuid::Uuid> {
    claims.employee_id.ok_or_else(|| {
        KabiPayError::Forbidden("An employee-linked account is required".into()).into_graphql()
    })
}

pub struct QueryRoot;

#[Object(name = "PerformanceQueryOperations")]
impl QueryRoot {
    async fn performance_health(&self) -> &'static str {
        "ok"
    }

    async fn review_cycles(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 20)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<ReviewCycleDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            return Err(
                KabiPayError::Forbidden("performance:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = performance_service::list_cycles(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ReviewCycleDto::from).collect())
    }

    async fn goals(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<GoalDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            return Err(
                KabiPayError::Forbidden("performance:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = performance_service::list_goals(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(GoalDto::from).collect())
    }

    async fn performance_programs(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<PerformanceProgramDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            return Err(KabiPayError::Forbidden(
                "performance:manage with ALL scope required".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        performance_workflow::list_programs(&db, tenant_id)
            .await
            .map(|rows| rows.into_iter().map(Into::into).collect())
            .map_err(KabiPayError::into_graphql)
    }

    async fn appraisal_templates(
        &self,
        ctx: &Context<'_>,
        performance_program_id: ID,
    ) -> Result<Vec<AppraisalTemplateDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            return Err(KabiPayError::Forbidden(
                "performance:manage with ALL scope required".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let program_id = parse_id(&performance_program_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let templates = performance_workflow::list_templates(&db, tenant_id, program_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let mut result = Vec::with_capacity(templates.len());
        for template in templates {
            result.push(
                performance_workflow::load_template(&db, tenant_id, template.id)
                    .await
                    .map_err(KabiPayError::into_graphql)?,
            );
        }
        Ok(result)
    }

    async fn my_performance_reviews(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<PerformanceReviewSummaryDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_use_performance_self_service() && !claims.can_manage_performance_programs() {
            return Err(KabiPayError::Forbidden(
                "performance:self with SELF scope required".into(),
            )
            .into_graphql());
        }
        let employee_id = require_employee_id(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        performance_workflow::list_reviews_for_employee(&db, tenant_id, employee_id)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn my_team_performance_reviews(
        &self,
        ctx: &Context<'_>,
    ) -> Result<Vec<PerformanceReviewSummaryDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_evaluate_performance_team() && !claims.can_manage_performance_programs() {
            return Err(KabiPayError::Forbidden(
                "performance:evaluate with TEAM scope required".into(),
            )
            .into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        if claims.can_manage_performance_programs() {
            performance_workflow::list_all_reviews(&db, tenant_id)
                .await
                .map_err(KabiPayError::into_graphql)
        } else {
            let employee_id = require_employee_id(claims)?;
            performance_workflow::list_reviews_for_manager(&db, tenant_id, employee_id)
                .await
                .map_err(KabiPayError::into_graphql)
        }
    }

    async fn performance_review_detail(
        &self,
        ctx: &Context<'_>,
        participant_id: ID,
    ) -> Result<PerformanceReviewDetailDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            require_employee_id(claims)?;
        }
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let participant = performance_workflow::load_participant(&db, tenant_id, participant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let actor_employee_id = claims.employee_id;
        let allowed = claims.can_manage_performance_programs()
            || (claims.can_use_performance_self_service()
                && actor_employee_id == Some(participant.employee_id))
            || (claims.can_evaluate_performance_team()
                && actor_employee_id.is_some_and(|actor| {
                    performance_lifecycle::manager_matches_snapshot(actor, participant.manager_employee_id)
                }));
        if !allowed {
            return Err(KabiPayError::Forbidden(
                "This performance review is outside your authorized scope".into(),
            )
            .into_graphql());
        }
        performance_workflow::load_review_detail(&db, tenant_id, participant)
            .await
            .map_err(KabiPayError::into_graphql)
    }
}
