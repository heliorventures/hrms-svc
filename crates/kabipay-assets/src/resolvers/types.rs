//! GraphQL DTOs for kabipay-assets.

use async_graphql::{InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_db_entities::tenant::d0022_assets::{asset, asset_allocation, asset_category};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AssetCategory")]
pub struct AssetCategoryDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<asset_category::Model> for AssetCategoryDto {
    fn from(m: asset_category::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            code: m.code,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Asset")]
pub struct AssetDto {
    pub id: ID,
    pub tenant_id: ID,
    pub asset_category_id: ID,
    pub name: String,
    pub serial_number: Option<String>,
    pub asset_tag: Option<String>,
    pub purchase_value: Option<String>,
    pub purchase_date: Option<NaiveDate>,
    pub status: String,
}

impl From<asset::Model> for AssetDto {
    fn from(m: asset::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            asset_category_id: ID(m.asset_category_id.to_string()),
            name: m.name,
            serial_number: m.serial_number,
            asset_tag: m.asset_tag,
            purchase_value: m.purchase_value.map(|d| d.to_string()),
            purchase_date: m.purchase_date,
            status: m.status,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AssetAssignment")]
pub struct AssetAssignmentDto {
    pub id: ID,
    pub asset_id: ID,
    pub employee_id: ID,
    pub asset_name: String,
    pub asset_tag: Option<String>,
    pub serial_number: Option<String>,
    pub purchase_value: Option<String>,
    pub allocated_on: NaiveDate,
    pub expected_return_on: Option<NaiveDate>,
    pub condition_at_allocation: Option<String>,
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
        Self {
            id: ID(allocation.id.to_string()),
            asset_id: ID(allocation.asset_id.to_string()),
            employee_id: ID(allocation.employee_id.to_string()),
            asset_name: asset.name,
            asset_tag: asset.asset_tag,
            serial_number: asset.serial_number,
            purchase_value: if include_purchase_value {
                asset.purchase_value.map(|d| d.to_string())
            } else {
                None
            },
            allocated_on: allocation.allocated_on,
            expected_return_on: allocation.expected_return_on,
            condition_at_allocation: allocation.condition_at_allocation,
            status: allocation.status,
            created_at: allocation.created_at,
            updated_at: allocation.updated_at,
        }
    }
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
