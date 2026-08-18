//! Tenant-scoped Asset Management query resolvers.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    context::PERM_ASSETS_SELF,
    subgraph::{require_client_claims, require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError, PageInput,
};
use uuid::Uuid;

use crate::resolvers::types::{
    AssetAllocationPage, AssetAssignmentDto, AssetCategoryDto, AssetCategoryPage, AssetDto,
    AssetEmployeeOption, AssetEmployeeOptionPage, AssetInventoryPage, AssetLocationOption,
};
use crate::resolvers::mutation::{
    has_explicit_asset_manage_permission, has_explicit_asset_read_permission,
};
use crate::services::asset_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|error| KabiPayError::Validation(format!("invalid {field}: {error}")).into_graphql())
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
        if !has_explicit_asset_read_permission(claims) {
            return Err(KabiPayError::Forbidden(
                "assets:read or assets:manage permission required".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = asset_service::list_categories(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AssetCategoryDto::from).collect())
    }

    async fn asset_categories_page(
        &self,
        ctx: &Context<'_>,
        page: Option<PageInput>,
        search: Option<String>,
        #[graphql(default = true)] active_only: bool,
    ) -> Result<AssetCategoryPage> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !has_explicit_asset_read_permission(claims) {
            return Err(KabiPayError::Forbidden(
                "assets:read or assets:manage permission required".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let result = asset_service::list_category_page(
            &db,
            tenant_id,
            page.unwrap_or_default(),
            search,
            active_only,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetCategoryPage {
            rows: result.rows.into_iter().map(AssetCategoryDto::from).collect(),
            page_info: result.page_info,
        })
    }

    async fn assets(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AssetDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !has_explicit_asset_read_permission(claims) {
            return Err(KabiPayError::Forbidden(
                "assets:read or assets:manage permission required".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = asset_service::list_assets(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(AssetDto::from).collect())
    }

    async fn asset_location_options(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AssetLocationOption>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !has_explicit_asset_read_permission(claims) {
            return Err(KabiPayError::Forbidden(
                "assets:read or assets:manage permission required".into(),
            )
            .into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = asset_service::list_location_options(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(|row| AssetLocationOption {
                id: ID(row.id.to_string()),
                name: row.name,
            })
            .collect())
    }

    async fn asset_employee_options_page(
        &self,
        ctx: &Context<'_>,
        page: Option<PageInput>,
        search: Option<String>,
    ) -> Result<AssetEmployeeOptionPage> {
        let claims = require_client_claims(ctx)?;
        if !has_explicit_asset_manage_permission(claims) {
            return Err(
                KabiPayError::Forbidden("assets:manage permission required".into()).into_graphql(),
            );
        }
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let result = asset_service::list_employee_option_page(
            &db,
            tenant_id,
            page.unwrap_or_default(),
            search,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetEmployeeOptionPage {
            rows: result
                .rows
                .into_iter()
                .map(AssetEmployeeOption::from)
                .collect(),
            page_info: result.page_info,
        })
    }

    async fn asset_inventory_page(
        &self,
        ctx: &Context<'_>,
        page: Option<PageInput>,
        search: Option<String>,
        category_id: Option<ID>,
        status: Option<String>,
    ) -> Result<AssetInventoryPage> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !has_explicit_asset_read_permission(claims) {
            return Err(KabiPayError::Forbidden(
                "assets:read or assets:manage permission required".into(),
            )
            .into_graphql());
        }
        let category_id = category_id
            .as_ref()
            .map(|id| parse_uuid(id, "categoryId"))
            .transpose()?;
        let db = tenant_db(ctx, tenant_id).await?;
        let result = asset_service::list_asset_page(
            &db,
            tenant_id,
            page.unwrap_or_default(),
            search,
            category_id,
            status,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetInventoryPage {
            rows: result
                .rows
                .into_iter()
                .map(|detail| AssetDto::from_parts(detail.asset, detail.category))
                .collect(),
            page_info: result.page_info,
        })
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
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let can_read_all = has_explicit_asset_read_permission(claims);
        let target_employee_id = if can_read_all {
            requested_employee_id
        } else {
            if !claims.has_any_permission(&[PERM_ASSETS_SELF]) {
                return Err(
                    KabiPayError::Forbidden("assets:self permission required".into()).into_graphql(),
                );
            }
            let own = resolve_client_employee_id(ctx, &db, tenant_id).await?;
            if requested_employee_id.is_some_and(|requested| requested != own) {
                return Err(KabiPayError::Forbidden(
                    "employees can view only their own assigned assets".into(),
                )
                .into_graphql());
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

    async fn asset_allocations_page(
        &self,
        ctx: &Context<'_>,
        page: Option<PageInput>,
        search: Option<String>,
        employee_id: Option<ID>,
        status: Option<String>,
    ) -> Result<AssetAllocationPage> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let can_read_all = has_explicit_asset_read_permission(claims);
        let target_employee_id = if can_read_all {
            requested_employee_id
        } else {
            if !claims.has_any_permission(&[PERM_ASSETS_SELF]) {
                return Err(
                    KabiPayError::Forbidden("assets:self permission required".into()).into_graphql(),
                );
            }
            let own = resolve_client_employee_id(ctx, &db, tenant_id).await?;
            if requested_employee_id.is_some_and(|requested| requested != own) {
                return Err(KabiPayError::Forbidden(
                    "employees can view only their own assigned assets".into(),
                )
                .into_graphql());
            }
            Some(own)
        };
        let result = asset_service::list_allocation_page(
            &db,
            tenant_id,
            page.unwrap_or_default(),
            search,
            target_employee_id,
            status,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetAllocationPage {
            rows: result
                .rows
                .into_iter()
                .map(|detail| {
                    AssetAssignmentDto::from_detail(
                        detail.allocation,
                        detail.asset,
                        detail.employee,
                        detail.return_log,
                        can_read_all,
                    )
                })
                .collect(),
            page_info: result.page_info,
        })
    }
}
