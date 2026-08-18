//! GraphQL DTOs for the tenant Asset Management lifecycle.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::PageInfo;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0022_assets::{
    asset, asset_allocation, asset_category, asset_return_log,
};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AssetCategory")]
pub struct AssetCategoryDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: Option<String>,
    pub is_active: bool,
    pub retired_at: Option<DateTime<Utc>>,
    pub retired_by: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<asset_category::Model> for AssetCategoryDto {
    fn from(model: asset_category::Model) -> Self {
        Self {
            id: ID(model.id.to_string()),
            tenant_id: ID(model.tenant_id.to_string()),
            name: model.name,
            code: model.code,
            is_active: model.is_active,
            retired_at: model.retired_at,
            retired_by: model.retired_by.map(|id| ID(id.to_string())),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Asset")]
pub struct AssetDto {
    pub id: ID,
    pub tenant_id: ID,
    pub asset_category_id: ID,
    pub category_name: Option<String>,
    pub category_code: Option<String>,
    pub name: String,
    pub serial_number: Option<String>,
    pub asset_tag: Option<String>,
    pub purchase_value: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub status: String,
    pub location_id: Option<ID>,
    pub retired_at: Option<DateTime<Utc>>,
    pub retired_by: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AssetDto {
    pub fn from_parts(model: asset::Model, category: Option<asset_category::Model>) -> Self {
        Self {
            id: ID(model.id.to_string()),
            tenant_id: ID(model.tenant_id.to_string()),
            asset_category_id: ID(model.asset_category_id.to_string()),
            category_name: category.as_ref().map(|row| row.name.clone()),
            category_code: category.and_then(|row| row.code),
            name: model.name,
            serial_number: model.serial_number,
            asset_tag: model.asset_tag,
            purchase_value: model.purchase_value.map(|value| value.to_string()),
            purchase_date: model.purchase_date,
            status: model.status,
            location_id: model.location_id.map(|id| ID(id.to_string())),
            retired_at: model.retired_at,
            retired_by: model.retired_by.map(|id| ID(id.to_string())),
            created_at: model.created_at,
            updated_at: model.updated_at,
        }
    }
}

impl From<asset::Model> for AssetDto {
    fn from(model: asset::Model) -> Self {
        Self::from_parts(model, None)
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AssetAssignment")]
pub struct AssetAssignmentDto {
    pub id: ID,
    pub asset_id: ID,
    pub employee_id: ID,
    pub employee_code: Option<String>,
    pub employee_name: Option<String>,
    pub asset_name: String,
    pub asset_tag: Option<String>,
    pub serial_number: Option<String>,
    pub purchase_value: Option<String>,
    pub allocated_on: NaiveDate,
    pub expected_return_on: Option<NaiveDate>,
    pub condition_at_allocation: Option<String>,
    pub returned_on: Option<NaiveDate>,
    pub condition_at_return: Option<String>,
    pub return_remarks: Option<String>,
    pub status: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl AssetAssignmentDto {
    pub fn from_parts(
        allocation: asset_allocation::Model,
        asset: asset::Model,
        include_purchase_value: bool,
    ) -> Self {
        Self::from_detail(allocation, asset, None, None, include_purchase_value)
    }

    pub fn from_detail(
        allocation: asset_allocation::Model,
        asset: asset::Model,
        employee: Option<employee::Model>,
        return_log: Option<asset_return_log::Model>,
        include_purchase_value: bool,
    ) -> Self {
        let employee_name = employee.as_ref().map(|row| {
            format!("{} {}", row.first_name.trim(), row.last_name.trim())
                .trim()
                .to_string()
        });
        Self {
            id: ID(allocation.id.to_string()),
            asset_id: ID(allocation.asset_id.to_string()),
            employee_id: ID(allocation.employee_id.to_string()),
            employee_code: employee.as_ref().map(|row| row.employee_code.clone()),
            employee_name,
            asset_name: asset.name,
            asset_tag: asset.asset_tag,
            serial_number: asset.serial_number,
            purchase_value: if include_purchase_value {
                asset.purchase_value.map(|value| value.to_string())
            } else {
                None
            },
            allocated_on: allocation.allocated_on,
            expected_return_on: allocation.expected_return_on,
            condition_at_allocation: allocation.condition_at_allocation,
            returned_on: return_log.as_ref().map(|row| row.returned_on),
            condition_at_return: return_log
                .as_ref()
                .and_then(|row| row.condition_at_return.clone()),
            return_remarks: return_log.and_then(|row| row.remarks),
            status: allocation.status,
            created_at: allocation.created_at,
            updated_at: allocation.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AssetCategoryPage {
    pub rows: Vec<AssetCategoryDto>,
    pub page_info: PageInfo,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AssetInventoryPage {
    pub rows: Vec<AssetDto>,
    pub page_info: PageInfo,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AssetAllocationPage {
    pub rows: Vec<AssetAssignmentDto>,
    pub page_info: PageInfo,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AssetLocationOption {
    pub id: ID,
    pub name: String,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AssetEmployeeOption {
    pub employee_id: ID,
    pub employee_code: String,
    pub full_name: String,
    pub status: String,
}

impl From<employee::Model> for AssetEmployeeOption {
    fn from(model: employee::Model) -> Self {
        let full_name = format!("{} {}", model.first_name.trim(), model.last_name.trim())
            .trim()
            .to_string();
        Self {
            employee_id: ID(model.id.to_string()),
            employee_code: model.employee_code,
            full_name,
            status: model.status,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
pub struct AssetEmployeeOptionPage {
    pub rows: Vec<AssetEmployeeOption>,
    pub page_info: PageInfo,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertAssetCategoryInput {
    pub id: Option<ID>,
    pub name: String,
    pub code: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertAssetInput {
    pub id: Option<ID>,
    pub asset_category_id: ID,
    pub name: String,
    pub serial_number: Option<String>,
    pub asset_tag: Option<String>,
    pub purchase_value: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub location_id: Option<ID>,
}

#[derive(InputObject, Clone, Debug)]
pub struct AssignAssetInput {
    pub asset_id: ID,
    pub employee_id: ID,
    pub allocated_on: NaiveDate,
    pub expected_return_on: Option<NaiveDate>,
    pub condition_at_allocation: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct ReturnAssetInput {
    pub asset_allocation_id: ID,
    pub returned_on: NaiveDate,
    pub condition_at_return: Option<String>,
    pub remarks: Option<String>,
}
