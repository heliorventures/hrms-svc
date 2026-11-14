//! Root query resolvers for kabipay-grievance.

use async_graphql::{Context, Object, Result};
use kabipay_common::{
    subgraph::{
        require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db,
    },
    KabiPayError,
};

use crate::resolvers::types::{GrievanceCaseDto, GrievanceCategoryDto};
use crate::services::grievance_service;

pub struct QueryRoot;

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Schema};
    use kabipay_common::{
        context::{ClientClaims, CLIENT_JWT_ISSUER},
        subgraph::TenantId,
    };
    use std::collections::HashMap;
    use uuid::Uuid;

    #[tokio::test]
    async fn grievance_reads_require_explicit_self_or_manage_all_before_database_access() {
        for permission in ["grievance:self", "grievance:manage"] {
            for scope in [
                None,
                Some("invalid"),
                Some("SELF"),
                Some("TEAM"),
                Some("DEPARTMENT"),
                Some("ALL"),
            ] {
                let tenant_id = Uuid::new_v4();
                let claims = ClientClaims {
                    sub: Uuid::new_v4(),
                    iss: CLIENT_JWT_ISSUER.into(),
                    exp: 0,
                    iat: 0,
                    tenant_id,
                    email: String::new(),
                    employee_id: None,
                    must_change_password: false,
                    roles: vec![],
                    permissions: vec![permission.into()],
                    permission_scopes: scope
                        .map(|s| HashMap::from([(permission.into(), s.into())]))
                        .unwrap_or_default(),
                    resource_scopes: HashMap::new(),
                };
                let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
                    .data(TenantId(tenant_id))
                    .data(claims)
                    .finish();
                for query in [
                    "{ grievanceCases { id } }",
                    "{ grievanceCategories { id } }",
                ] {
                    let response = schema.execute(query).await;
                    assert_eq!(response.errors.len(), 1, "{response:?}");
                    let message = &response.errors[0].message;
                    let allowed = (permission == "grievance:self" && scope == Some("SELF"))
                        || (permission == "grievance:manage" && scope == Some("ALL"));
                    assert_eq!(
                        message == "internal server error",
                        allowed,
                        "{query} {permission} {scope:?}: {message}"
                    );
                    if !allowed {
                        assert!(message.contains("permission"), "{message}");
                    }
                }
            }
        }
    }
}

#[Object]
impl QueryRoot {
    async fn grievance_health(&self) -> &'static str {
        "ok"
    }

    async fn grievance_categories(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<GrievanceCategoryDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_use_grievance_self_service() && !claims.can_manage_grievance_tenant_cases() {
            return Err(
                KabiPayError::Forbidden("grievance:self or grievance:manage permission required".into())
                    .into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = grievance_service::list_categories(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(GrievanceCategoryDto::from).collect())
    }

    /// `grievance:manage` sees tenant-wide cases; others see **their own** cases only.
    async fn grievance_cases(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<GrievanceCaseDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_use_grievance_self_service() && !claims.can_manage_grievance_tenant_cases() {
            return Err(
                KabiPayError::Forbidden("grievance:self or grievance:manage permission required".into())
                    .into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let filter = if claims.can_manage_grievance_tenant_cases() {
            None
        } else {
            Some(
                resolve_client_employee_id(ctx, &db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)?,
            )
        };
        let rows = grievance_service::list_cases(&db, tenant_id, limit, filter)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(GrievanceCaseDto::from).collect())
    }
}
