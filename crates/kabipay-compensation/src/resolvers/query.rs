//! Root query resolvers for kabipay-compensation.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{CompensationReviewCycleDto, SalaryBandDto};
use crate::services::compensation_service;

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn compensation_health(&self) -> &'static str {
        "ok"
    }

    async fn salary_bands(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<SalaryBandDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_compensation_admin() {
            return Err(
                KabiPayError::Forbidden("compensation:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = compensation_service::list_bands(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(SalaryBandDto::from).collect())
    }

    async fn compensation_review_cycles(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 20)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<CompensationReviewCycleDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_compensation_admin() {
            return Err(
                KabiPayError::Forbidden("compensation:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = compensation_service::list_cycles(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(CompensationReviewCycleDto::from)
            .collect())
    }
}
