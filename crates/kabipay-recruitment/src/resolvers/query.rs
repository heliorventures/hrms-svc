//! Root query resolvers for kabipay-recruitment.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{ApplicationDto, JobPostingDto};
use crate::services::recruitment_service;

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn recruitment_health(&self) -> &'static str {
        "ok"
    }

    async fn job_postings(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<JobPostingDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_recruitment() {
            return Err(
                KabiPayError::Forbidden("recruitment:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = recruitment_service::list_jobs(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(JobPostingDto::from).collect())
    }

    async fn applications(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<ApplicationDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_recruitment() {
            return Err(
                KabiPayError::Forbidden("recruitment:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = recruitment_service::list_applications(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(ApplicationDto::from).collect())
    }
}
