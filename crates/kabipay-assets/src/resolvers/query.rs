//! Tenant-scoped Asset Management query resolvers.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{resolve_employee_scope_filter, resolve_viewer_employee, EmployeeScopeFilter},
    context::{ClientClaims, ScopeType, PERM_ASSETS_READ, PERM_ASSETS_SELF},
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
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

fn asset_read_scope(claims: &ClientClaims, allow_self_service: bool) -> Result<ScopeType> {
    if has_explicit_asset_read_permission(claims) {
        return Ok(ScopeType::All);
    }
    if claims.has_any_permission(&[PERM_ASSETS_READ]) {
        if let Some(scope) = claims.scope_for_permission(PERM_ASSETS_READ) {
            return Ok(scope);
        }
    }
    if allow_self_service
        && claims.has_any_permission(&[PERM_ASSETS_SELF])
        && claims.scope_for_permission(PERM_ASSETS_SELF) == Some(ScopeType::Self_)
    {
        return Ok(ScopeType::Self_);
    }
    Err(KabiPayError::Forbidden(
        "assets:read or assets:manage requires explicit authorized scope".into(),
    )
    .into_graphql())
}

async fn asset_employee_filter(
    ctx: &Context<'_>,
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    scope: ScopeType,
    requested_employee_id: Option<Uuid>,
) -> Result<EmployeeScopeFilter> {
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer_employee(ctx, db, tenant_id).await?
    };
    let filter = resolve_employee_scope_filter(db, tenant_id, scope, viewer)
        .await
        .map_err(KabiPayError::into_graphql)?;
    restrict_requested_employee(filter, requested_employee_id)
}

fn restrict_requested_employee(
    filter: EmployeeScopeFilter,
    requested_employee_id: Option<Uuid>,
) -> Result<EmployeeScopeFilter> {
    if let Some(employee_id) = requested_employee_id {
        if !filter.allows_employee(employee_id) {
            return Err(KabiPayError::Forbidden(
                "employee is outside the authorized assets scope".into(),
            )
            .into_graphql());
        }
        return Ok(EmployeeScopeFilter::EmployeeIds(vec![employee_id]));
    }
    Ok(filter)
}

fn asset_for_scope(mut dto: AssetDto, scope: ScopeType) -> AssetDto {
    if scope != ScopeType::All {
        dto.purchase_value = None;
    }
    dto
}

#[cfg(test)]
mod scope_tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Schema};
    use kabipay_common::{
        context::{CLIENT_JWT_ISSUER, PERM_ASSETS_MANAGE},
        subgraph::TenantId,
    };
    use std::collections::HashMap;

    #[test]
    fn bounded_asset_inventory_responses_hide_purchase_values() {
        let dto = AssetDto {
            id: ID(Uuid::new_v4().to_string()),
            tenant_id: ID(Uuid::new_v4().to_string()),
            asset_category_id: ID(Uuid::new_v4().to_string()),
            category_name: None,
            category_code: None,
            name: "Laptop".into(),
            serial_number: None,
            asset_tag: None,
            purchase_value: Some("1000".into()),
            purchase_date: None,
            status: "ASSIGNED".into(),
            location_id: None,
            retired_at: None,
            retired_by: None,
            created_at: chrono::Utc::now(),
            updated_at: chrono::Utc::now(),
        };
        for scope in [ScopeType::Self_, ScopeType::Team, ScopeType::Department] {
            let result = asset_for_scope(dto.clone(), scope);
            assert_eq!(result.id, dto.id);
            assert_eq!(result.name, "Laptop");
            assert_eq!(result.purchase_value, None);
        }
        assert_eq!(asset_for_scope(dto, ScopeType::All).purchase_value.as_deref(), Some("1000"));
    }

    #[test]
    fn requested_employee_cannot_replace_or_widen_the_resolved_asset_population() {
        let own = Uuid::new_v4();
        let other = Uuid::new_v4();
        let filter = EmployeeScopeFilter::EmployeeIds(vec![own]);
        assert!(restrict_requested_employee(filter.clone(), Some(other)).is_err());
        assert!(restrict_requested_employee(EmployeeScopeFilter::Empty, Some(own)).is_err());
        let narrowed = restrict_requested_employee(filter, Some(own)).unwrap();
        assert!(narrowed.allows_employee(own));
        assert!(!narrowed.allows_employee(other));
        let narrowed =
            restrict_requested_employee(EmployeeScopeFilter::Unrestricted, Some(own)).unwrap();
        assert!(narrowed.allows_employee(own));
        assert!(!narrowed.allows_employee(other));
        assert!(matches!(
            restrict_requested_employee(EmployeeScopeFilter::Empty, None).unwrap(),
            EmployeeScopeFilter::Empty
        ));
    }

    #[tokio::test]
    async fn asset_reads_preserve_each_explicit_read_scope_and_reject_invalid_management() {
        for permission in [PERM_ASSETS_READ, PERM_ASSETS_MANAGE, PERM_ASSETS_SELF] {
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
                let expected = if permission == PERM_ASSETS_READ {
                    scope.and_then(ScopeType::parse_loose)
                } else if permission == PERM_ASSETS_MANAGE && scope == Some("ALL") {
                    Some(ScopeType::All)
                } else if permission == PERM_ASSETS_SELF && scope == Some("SELF") {
                    Some(ScopeType::Self_)
                } else {
                    None
                };
                assert_eq!(
                    asset_read_scope(&claims, true).ok(),
                    expected,
                    "{permission} {scope:?}"
                );
                let schema = Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
                    .data(TenantId(tenant_id))
                    .data(claims)
                    .finish();
                for query in [
                    "{ assetAssignments { id } }",
                    "{ assetAllocationsPage { rows { id } } }",
                ] {
                    let response = schema.execute(query).await;
                    assert_eq!(response.errors.len(), 1, "{response:?}");
                    let message = &response.errors[0].message;
                    assert_eq!(
                        message == "internal server error",
                        expected.is_some(),
                        "{query} {permission} {scope:?}: {message}"
                    );
                }
            }
        }
    }
}

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
        let _scope = asset_read_scope(claims, false)?;
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
        let _scope = asset_read_scope(claims, false)?;
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
        let scope = asset_read_scope(claims, false)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let filter = asset_employee_filter(ctx, &db, tenant_id, scope, None).await?;
        let rows = asset_service::list_assets(&db, tenant_id, limit, &filter)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(|row| asset_for_scope(AssetDto::from(row), scope)).collect())
    }

    async fn asset_location_options(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<AssetLocationOption>> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        let _scope = asset_read_scope(claims, false)?;
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
        let scope = asset_read_scope(claims, false)?;
        let category_id = category_id
            .as_ref()
            .map(|id| parse_uuid(id, "categoryId"))
            .transpose()?;
        let db = tenant_db(ctx, tenant_id).await?;
        let filter = asset_employee_filter(ctx, &db, tenant_id, scope, None).await?;
        let result = asset_service::list_asset_page(
            &db,
            tenant_id,
            page.unwrap_or_default(),
            search,
            category_id,
            status,
            &filter,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        Ok(AssetInventoryPage {
            rows: result
                .rows
                .into_iter()
                .map(|detail| asset_for_scope(AssetDto::from_parts(detail.asset, detail.category), scope))
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
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let scope = asset_read_scope(claims, true)?;
        let can_read_all = scope == ScopeType::All;
        let db = tenant_db(ctx, tenant_id).await?;
        let filter = asset_employee_filter(ctx, &db, tenant_id, scope, requested_employee_id).await?;
        let rows = asset_service::list_asset_assignments(
            &db,
            tenant_id,
            &filter,
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
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let scope = asset_read_scope(claims, true)?;
        let can_read_all = scope == ScopeType::All;
        let db = tenant_db(ctx, tenant_id).await?;
        let filter = asset_employee_filter(ctx, &db, tenant_id, scope, requested_employee_id).await?;
        let result = asset_service::list_allocation_page(
            &db,
            tenant_id,
            page.unwrap_or_default(),
            search,
            &filter,
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
