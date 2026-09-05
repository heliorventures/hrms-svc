//! Root query resolvers for kabipay-succession.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};

use crate::resolvers::types::{CompetencyDto, TalentPoolDto};
use crate::services::succession_service;

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn succession_health(&self) -> &'static str {
        "ok"
    }

    async fn competencies(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<CompetencyDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_succession_planning() {
            return Err(
                KabiPayError::Forbidden("succession:manage with ALL scope required".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = succession_service::list_competencies(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(CompetencyDto::from).collect())
    }

    async fn talent_pools(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
        #[graphql(default = 0)] offset: u64,
    ) -> Result<Vec<TalentPoolDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_succession_planning() {
            return Err(
                KabiPayError::Forbidden("succession:manage with ALL scope required".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = succession_service::list_pools(&db, tenant_id, limit, offset)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TalentPoolDto::from).collect())
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use kabipay_common::{context::ClientClaims, subgraph::TenantId};
    use uuid::Uuid;

    #[tokio::test]
    async fn competency_catalog_requires_all_scoped_management_before_database_access() {
        let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription).finish();
        for (permissions, scopes) in [
            (vec!["succession:manage"], serde_json::json!({"succession:manage":"TEAM"})),
            (vec!["succession:manage"], serde_json::json!({})),
            (vec![], serde_json::json!({"succession:manage":"ALL"})),
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
                    Request::new("{ competencies { id } }")
                        .data(TenantId(Uuid::nil()))
                        .data(claims),
                )
                .await;
            assert_eq!(response.errors.len(), 1);
            assert!(
                response.errors[0].message.contains("ALL scope required"),
                "{:?}",
                response.errors
            );
        }
    }
}
