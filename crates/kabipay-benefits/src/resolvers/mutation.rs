//! Self-service benefit enrollment mutations.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::BenefitEnrollmentDto;
use crate::services::benefits_service;

pub struct MutationRoot;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

#[Object]
impl MutationRoot {
 async fn save_benefit_type(&self, ctx: &Context<'_>, id: Option<ID>, input: super::setup::BenefitTypeInput) -> Result<super::types::BenefitTypeDto> { super::setup::save_type(ctx,id,input).await }
 async fn save_benefit_plan(&self, ctx: &Context<'_>, id: Option<ID>, input: super::setup::BenefitPlanInput) -> Result<super::types::BenefitPlanDto> { super::setup::save_plan(ctx,id,input).await }

    /// Enroll the signed-in employee in an **active** benefit plan (`CONFLICT` if already enrolled).
    async fn enroll_in_benefit_plan(
        &self,
        ctx: &Context<'_>,
        benefit_plan_id: ID,
    ) -> Result<BenefitEnrollmentDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_read_benefit_catalog_queries() {
            return Err(
                KabiPayError::Forbidden("benefits:self or benefits:manage permission required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let employee_id = resolve_client_employee_id(ctx, &db, tenant_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let pid = parse_uuid(&benefit_plan_id, "benefitPlanId")?;
        let m =
            benefits_service::enroll_in_benefit_plan(&db, tenant_id, employee_id, pid)
                .await
                .map_err(KabiPayError::into_graphql)?;
        Ok(BenefitEnrollmentDto::from(m))
    }
}
