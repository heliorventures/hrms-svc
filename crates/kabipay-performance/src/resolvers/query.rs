//! Root query resolvers for kabipay-performance.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{GoalDto, ReviewCycleDto};
use crate::services::performance_service;

pub struct QueryRoot;

#[Object]
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
}
