//! Root query resolvers for kabipay-lms.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{CourseDto, SkillDto};
use crate::services::lms_service;

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn lms_health(&self) -> &'static str {
        "ok"
    }

    async fn skills(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<SkillDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_learning_catalog() {
            return Err(
                KabiPayError::Forbidden("learning:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = lms_service::list_skills(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(SkillDto::from).collect())
    }

    async fn courses(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 100)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<CourseDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_learning_catalog() {
            return Err(
                KabiPayError::Forbidden("learning:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = lms_service::list_courses(&db, tenant_id, active_only, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(CourseDto::from).collect())
    }
}
