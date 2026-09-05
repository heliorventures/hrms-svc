//! Root query resolvers for kabipay-benefits.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{BenefitEnrollmentDto, BenefitPlanDto, BenefitTypeDto};
use crate::services::benefits_service;

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn benefits_health(&self) -> &'static str {
        "ok"
    }

    async fn benefit_types(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<BenefitTypeDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_read_benefit_catalog_queries() {
            return Err(
                KabiPayError::Forbidden("benefits:self or benefits:manage permission required".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = benefits_service::list_types(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(BenefitTypeDto::from).collect())
    }

    async fn benefit_plans(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 50)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<BenefitPlanDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_read_benefit_catalog_queries() {
            return Err(
                KabiPayError::Forbidden("benefits:self or benefits:manage permission required".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = benefits_service::list_plans(&db, tenant_id, active_only, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(BenefitPlanDto::from).collect())
    }

    /// Signed-in employee's enrollments (`[]` until they enroll via `enroll_in_benefit_plan`).
    async fn my_benefit_enrollments(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<BenefitEnrollmentDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_use_benefits_self_service() {
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
        let rows = benefits_service::list_enrollments_for_employee(&db, tenant_id, employee_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        let names = benefits_service::plan_names(
            &db,
            tenant_id,
            rows.iter().map(|row| row.benefit_plan_id).collect(),
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|row| {
                let name = names.get(&row.benefit_plan_id).cloned();
                let mut dto = BenefitEnrollmentDto::from(row);
                dto.benefit_plan_name = name;
                dto
            })
            .collect())
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use kabipay_common::{context::ClientClaims, subgraph::TenantId};
    use uuid::Uuid;

    #[tokio::test]
    async fn own_enrollments_require_benefits_self_service_before_database_access() {
        let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();
        for (permissions, scopes) in [
            (vec!["benefits:self"], serde_json::json!({"benefits:self":"TEAM"})),
            (vec!["benefits:self"], serde_json::json!({})),
            (vec![], serde_json::json!({"benefits:self":"SELF"})),
        ] {
            let claims: ClientClaims = serde_json::from_value(serde_json::json!({
                "sub": Uuid::nil(),
                "iss": "test",
                "exp": 0,
                "iat": 0,
                "tenant_id": Uuid::nil(),
                "email": "test@example.com",
                "roles": [],
                "permissions": permissions,
                "permission_scopes": scopes,
                "resource_scopes": {}
            }))
            .expect("test claims");
            let response = schema
                .execute(
                    Request::new("{ myBenefitEnrollments { id } }")
                        .data(TenantId(Uuid::nil()))
                        .data(claims),
                )
                .await;
            assert_eq!(response.errors.len(), 1);
            assert!(
                response.errors[0].message.contains("permission required"),
                "{:?}",
                response.errors
            );
        }
    }
}
