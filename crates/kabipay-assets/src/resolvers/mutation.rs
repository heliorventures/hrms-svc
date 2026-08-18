//! Asset Management lifecycle mutations.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    context::{ClientClaims, PERM_ASSETS_MANAGE, PERM_ASSETS_READ},
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use sea_orm::prelude::Decimal;
use uuid::Uuid;

use crate::resolvers::types::{
    AssignAssetInput, AssetAssignmentDto, AssetCategoryDto, AssetDto, ReturnAssetInput,
    UpsertAssetCategoryInput, UpsertAssetInput,
};
use crate::services::asset_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|error| KabiPayError::Validation(format!("invalid {field}: {error}")).into_graphql())
}

fn parse_optional_uuid(id: Option<&ID>, field: &'static str) -> Result<Option<Uuid>> {
    id.map(|value| parse_uuid(value, field)).transpose()
}

fn parse_decimal(value: Option<String>) -> Result<Option<Decimal>> {
    value
        .map(|raw| {
            raw.trim().parse::<Decimal>().map_err(|_| {
                KabiPayError::Validation("purchaseValue must be a valid decimal number".into())
                    .into_graphql()
            })
        })
        .transpose()
}

pub(super) fn has_explicit_asset_manage_permission(claims: &ClientClaims) -> bool {
    claims.has_any_permission(&[PERM_ASSETS_MANAGE])
}

pub(super) fn has_explicit_asset_read_permission(claims: &ClientClaims) -> bool {
    claims.has_any_permission(&[PERM_ASSETS_READ, PERM_ASSETS_MANAGE])
}

fn require_asset_manager(ctx: &Context<'_>) -> Result<Uuid> {
    let claims = require_client_claims(ctx)?;
    if !has_explicit_asset_manage_permission(claims) {
        return Err(
            KabiPayError::Forbidden("assets:manage permission required".into()).into_graphql(),
        );
    }
    Ok(claims.sub)
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn upsert_asset_category(
        &self,
        ctx: &Context<'_>,
        input: UpsertAssetCategoryInput,
    ) -> Result<AssetCategoryDto> {
        require_asset_manager(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let id = parse_optional_uuid(input.id.as_ref(), "assetCategoryId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let row = asset_service::upsert_asset_category(
            &db,
            tenant_id,
            id,
            input.name,
            input.code,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetCategoryDto::from(row))
    }

    async fn retire_asset_category(
        &self,
        ctx: &Context<'_>,
        asset_category_id: ID,
    ) -> Result<AssetCategoryDto> {
        let acting_user_id = require_asset_manager(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let category_id = parse_uuid(&asset_category_id, "assetCategoryId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let row = asset_service::retire_asset_category(
            &db,
            tenant_id,
            category_id,
            acting_user_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetCategoryDto::from(row))
    }

    async fn upsert_asset(
        &self,
        ctx: &Context<'_>,
        input: UpsertAssetInput,
    ) -> Result<AssetDto> {
        require_asset_manager(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let id = parse_optional_uuid(input.id.as_ref(), "assetId")?;
        let category_id = parse_uuid(&input.asset_category_id, "assetCategoryId")?;
        let location_id = parse_optional_uuid(input.location_id.as_ref(), "locationId")?;
        let purchase_value = parse_decimal(input.purchase_value)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let row = asset_service::upsert_asset(
            &db,
            tenant_id,
            id,
            category_id,
            input.name,
            input.serial_number,
            input.asset_tag,
            purchase_value,
            input.purchase_date,
            location_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetDto::from(row))
    }

    async fn retire_asset(&self, ctx: &Context<'_>, asset_id: ID) -> Result<AssetDto> {
        let acting_user_id = require_asset_manager(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let asset_id = parse_uuid(&asset_id, "assetId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let row = asset_service::retire_asset(&db, tenant_id, asset_id, acting_user_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(AssetDto::from(row))
    }

    async fn assign_asset_to_employee(
        &self,
        ctx: &Context<'_>,
        input: AssignAssetInput,
    ) -> Result<AssetAssignmentDto> {
        require_asset_manager(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let asset_id = parse_uuid(&input.asset_id, "assetId")?;
        let employee_id = parse_uuid(&input.employee_id, "employeeId")?;
        let db = tenant_db(ctx, tenant_id).await?;
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
        let acting_user_id = require_asset_manager(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let allocation_id = parse_uuid(&input.asset_allocation_id, "assetAllocationId")?;
        let db = tenant_db(ctx, tenant_id).await?;
        let (allocation, asset) = asset_service::return_asset(
            &db,
            tenant_id,
            allocation_id,
            input.returned_on,
            input.condition_at_return,
            input.remarks,
            acting_user_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetAssignmentDto::from_parts(allocation, asset, true))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
    use std::collections::HashMap;

    fn claims(roles: &[&str], permissions: &[&str]) -> ClientClaims {
        ClientClaims {
            sub: Uuid::nil(),
            iss: CLIENT_JWT_ISSUER.to_string(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::nil(),
            email: String::new(),
            employee_id: None,
            must_change_password: false,
            roles: roles.iter().map(|role| (*role).to_string()).collect(),
            permissions: permissions
                .iter()
                .map(|permission| (*permission).to_string())
                .collect(),
            resource_scopes: HashMap::new(),
        }
    }

    #[test]
    fn asset_mutations_reject_role_name_without_explicit_manage_permission() {
        let claims = claims(&["HR_ADMIN", "TENANT_ADMIN", "ORG_ADMIN"], &[]);

        assert!(!has_explicit_asset_manage_permission(&claims));
    }

    #[test]
    fn asset_mutations_accept_explicit_manage_permission() {
        let claims = claims(&[], &["assets:manage"]);

        assert!(has_explicit_asset_manage_permission(&claims));
    }

    #[test]
    fn tenant_asset_reads_reject_role_names_without_explicit_permission() {
        let claims = claims(&["HR_ADMIN", "TENANT_ADMIN", "ORG_ADMIN"], &[]);

        assert!(!has_explicit_asset_read_permission(&claims));
    }

    #[test]
    fn tenant_asset_reads_accept_explicit_read_or_manage_permission() {
        assert!(has_explicit_asset_read_permission(&claims(
            &[],
            &["assets:read"]
        )));
        assert!(has_explicit_asset_read_permission(&claims(
            &[],
            &["assets:manage"]
        )));
    }
}
