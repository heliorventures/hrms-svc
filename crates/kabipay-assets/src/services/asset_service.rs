//! Tenant-scoped SeaORM queries for assets.

use kabipay_common::{KabiPayError, KabiPayResult};
use chrono::{NaiveDate, Utc};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0022_assets::{asset, asset_allocation, asset_category, asset_return_log};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder,
    QuerySelect, Set, TransactionTrait,
};
use std::collections::HashMap;
use uuid::Uuid;

pub async fn list_categories(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<asset_category::Model>> {
    let limit = limit.clamp(1, 200);
    asset_category::Entity::find()
        .filter(asset_category::Column::TenantId.eq(tenant_id))
        .order_by_asc(asset_category::Column::Name)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_assets(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<asset::Model>> {
    let limit = limit.clamp(1, 500);
    asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .order_by_desc(asset::Column::CreatedAt)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_asset_assignments(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Option<Uuid>,
    active_only: bool,
    limit: u64,
) -> KabiPayResult<Vec<(asset_allocation::Model, asset::Model)>> {
    let limit = limit.clamp(1, 500);
    let mut q = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id));
    if let Some(employee_id) = employee_id {
        q = q.filter(asset_allocation::Column::EmployeeId.eq(employee_id));
    }
    if active_only {
        q = q.filter(asset_allocation::Column::Status.eq("ACTIVE"));
    }
    let allocations = q
        .order_by_desc(asset_allocation::Column::AllocatedOn)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    if allocations.is_empty() {
        return Ok(vec![]);
    }
    let asset_ids = allocations.iter().map(|row| row.asset_id).collect::<Vec<_>>();
    let assets = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.is_in(asset_ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let asset_map = assets.into_iter().map(|row| (row.id, row)).collect::<HashMap<_, _>>();
    Ok(allocations
        .into_iter()
        .filter_map(|allocation| {
            asset_map
                .get(&allocation.asset_id)
                .cloned()
                .map(|asset| (allocation, asset))
        })
        .collect())
}

pub async fn assign_asset(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    asset_id: Uuid,
    employee_id: Uuid,
    allocated_on: NaiveDate,
    expected_return_on: Option<NaiveDate>,
    condition_at_allocation: Option<String>,
) -> KabiPayResult<(asset_allocation::Model, asset::Model)> {
    if let Some(expected) = expected_return_on {
        if expected < allocated_on {
            return Err(KabiPayError::Validation(
                "expectedReturnOn cannot be before allocatedOn".into(),
            ));
        }
    }
    let now = Utc::now();
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let asset_row = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.eq(asset_id))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset",
            id: asset_id.to_string(),
        })?;
    if asset_row.status.eq_ignore_ascii_case("ASSIGNED") {
        return Err(KabiPayError::Conflict("asset is already assigned".into()));
    }
    let has_active_allocation = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id))
        .filter(asset_allocation::Column::AssetId.eq(asset_id))
        .filter(asset_allocation::Column::Status.eq("ACTIVE"))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .is_some();
    if has_active_allocation {
        return Err(KabiPayError::Conflict("asset already has an active allocation".into()));
    }
    let employee_exists = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.eq(employee_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .is_some();
    if !employee_exists {
        return Err(KabiPayError::NotFound {
            entity: "employee",
            id: employee_id.to_string(),
        });
    }
    let allocation = asset_allocation::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        asset_id: Set(asset_id),
        employee_id: Set(employee_id),
        allocated_on: Set(allocated_on),
        expected_return_on: Set(expected_return_on),
        condition_at_allocation: Set(condition_at_allocation.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        })),
        status: Set("ACTIVE".to_string()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(KabiPayError::from)?;
    let mut active_asset: asset::ActiveModel = asset_row.into();
    active_asset.status = Set("ASSIGNED".to_string());
    active_asset.updated_at = Set(now);
    let updated_asset = active_asset.update(&txn).await.map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok((allocation, updated_asset))
}

pub async fn return_asset(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    allocation_id: Uuid,
    returned_on: NaiveDate,
    condition_at_return: Option<String>,
    remarks: Option<String>,
    received_by: Uuid,
) -> KabiPayResult<(asset_allocation::Model, asset::Model)> {
    let now = Utc::now();
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let allocation = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id))
        .filter(asset_allocation::Column::Id.eq(allocation_id))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset_allocation",
            id: allocation_id.to_string(),
        })?;
    if !allocation.status.eq_ignore_ascii_case("ACTIVE") {
        return Err(KabiPayError::Conflict("asset allocation is not active".into()));
    }
    if returned_on < allocation.allocated_on {
        return Err(KabiPayError::Validation(
            "returnedOn cannot be before allocatedOn".into(),
        ));
    }
    let asset_row = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.eq(allocation.asset_id))
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset",
            id: allocation.asset_id.to_string(),
        })?;
    asset_return_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        asset_allocation_id: Set(allocation.id),
        returned_on: Set(returned_on),
        condition_at_return: Set(condition_at_return.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        })),
        remarks: Set(remarks.and_then(|s| {
            let t = s.trim().to_string();
            if t.is_empty() { None } else { Some(t) }
        })),
        received_by: Set(Some(received_by)),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(KabiPayError::from)?;
    let mut active_allocation: asset_allocation::ActiveModel = allocation.into();
    active_allocation.status = Set("RETURNED".to_string());
    active_allocation.updated_at = Set(now);
    let updated_allocation = active_allocation.update(&txn).await.map_err(KabiPayError::from)?;
    let mut active_asset: asset::ActiveModel = asset_row.into();
    active_asset.status = Set("AVAILABLE".to_string());
    active_asset.updated_at = Set(now);
    let updated_asset = active_asset.update(&txn).await.map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok((updated_allocation, updated_asset))
}
