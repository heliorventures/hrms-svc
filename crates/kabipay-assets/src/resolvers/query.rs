//! Root query resolvers for kabipay-assets.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{AssetAssignmentDto, AssetCategoryDto, AssetDto};
use crate::services::asset_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn assets_health(&self) -> &'static str {
        "ok"
    }

    async fn asset_categories(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 50)] limit: u64,
    ) -> Result<Vec<AssetCategoryDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_read_assets_registry() {
            return Err(
                KabiPayError::Forbidden("assets:read or assets:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = asset_service::list_categories(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AssetCategoryDto::from).collect())
    }

    async fn assets(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AssetDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.can_read_assets_registry() {
            return Err(
                KabiPayError::Forbidden("assets:read or assets:manage permission required".into()).into_graphql(),
            );
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = asset_service::list_assets(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AssetDto::from).collect())
    }

    async fn asset_assignments(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AssetAssignmentDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let viewer_employee_id = resolve_client_employee_id(ctx, &db, tenant_id).await.ok();
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let can_read_all = claims.can_read_assets_registry();
        let target_employee_id = if can_read_all {
            requested_employee_id
        } else {
            if !claims.can_read_own_assets() {
                return Err(
                    KabiPayError::Forbidden("assets:self permission required".into()).into_graphql(),
                );
            }
            let own = viewer_employee_id.ok_or_else(|| {
                KabiPayError::Forbidden("signed-in user is not linked to an employee".into())
                    .into_graphql()
            })?;
            if let Some(requested) = requested_employee_id {
                if requested != own {
                    return Err(
                        KabiPayError::Forbidden("employees can view only their own assigned assets".into())
                            .into_graphql(),
                    );
                }
            }
            Some(own)
        };
        let rows = asset_service::list_asset_assignments(
            &db,
            tenant_id,
            target_employee_id,
            active_only,
            limit,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|(allocation, asset)| {
                AssetAssignmentDto::from_parts(allocation, asset, can_read_all)
            })
            .collect())
    }
}
