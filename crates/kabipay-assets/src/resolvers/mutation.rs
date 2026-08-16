//! Write resolvers for asset assignment and returns.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use uuid::Uuid;

use crate::resolvers::types::{AssignAssetInput, AssetAssignmentDto, ReturnAssetInput};
use crate::services::asset_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn assign_asset_to_employee(
        &self,
        ctx: &Context<'_>,
        input: AssignAssetInput,
    ) -> Result<AssetAssignmentDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_assets_registry() {
            return Err(
                KabiPayError::Forbidden("assets:manage permission required".into()).into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let asset_id = parse_uuid(&input.asset_id, "assetId")?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        let (allocation, asset) = asset_service::assign_asset(
            &db,
            tenant_id,
            asset_id,
            employee_id,
            input.allocated_on,
            input.expected_return_on,
            input.condition_at_allocation,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetAssignmentDto::from_parts(allocation, asset, true))
    }

    async fn return_employee_asset(
        &self,
        ctx: &Context<'_>,
        input: ReturnAssetInput,
    ) -> Result<AssetAssignmentDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_assets_registry() {
            return Err(
                KabiPayError::Forbidden("assets:manage permission required".into()).into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let allocation_id = parse_uuid(&input.asset_allocation_id, "assetAllocationId")?;
        let (allocation, asset) = asset_service::return_asset(
            &db,
            tenant_id,
            allocation_id,
            input.returned_on,
            input.condition_at_return,
            input.remarks,
            claims.sub,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetAssignmentDto::from_parts(allocation, asset, true))
    }
}
