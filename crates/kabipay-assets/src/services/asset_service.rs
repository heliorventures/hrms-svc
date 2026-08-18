//! Tenant-scoped persistence and lifecycle rules for Asset Management.

use std::collections::HashMap;

use chrono::{NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult, PageInfo, PageInput};
use kabipay_db_entities::tenant::d0006_org_hierarchy::location;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0022_assets::{
    asset, asset_allocation, asset_category, asset_return_log,
};
use sea_orm::prelude::Decimal;
use sea_orm::sea_query::Expr;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, Condition, DatabaseConnection, DbErr, EntityTrait,
    PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Select, Set, TransactionTrait,
};
use uuid::Uuid;

use crate::services::asset_rules::{
    category_code, optional_identifier, required_text, validate_purchase_value,
};

const ASSET_AVAILABLE: &str = "AVAILABLE";
const ASSET_ASSIGNED: &str = "ASSIGNED";
const ASSET_RETIRED: &str = "RETIRED";
const ALLOCATION_ACTIVE: &str = "ACTIVE";
const ALLOCATION_RETURNED: &str = "RETURNED";

pub struct CategoryPageData {
    pub rows: Vec<asset_category::Model>,
    pub page_info: PageInfo,
}

pub struct AssetDetail {
    pub asset: asset::Model,
    pub category: Option<asset_category::Model>,
}

pub struct AssetPageData {
    pub rows: Vec<AssetDetail>,
    pub page_info: PageInfo,
}

pub struct AllocationDetail {
    pub allocation: asset_allocation::Model,
    pub asset: asset::Model,
    pub employee: Option<employee::Model>,
    pub return_log: Option<asset_return_log::Model>,
}

pub struct AllocationPageData {
    pub rows: Vec<AllocationDetail>,
    pub page_info: PageInfo,
}

pub struct EmployeeOptionPageData {
    pub rows: Vec<employee::Model>,
    pub page_info: PageInfo,
}

fn search_pattern(search: Option<String>) -> Option<String> {
    search.and_then(|raw| {
        let normalized = raw.trim().to_ascii_lowercase();
        (!normalized.is_empty()).then(|| format!("%{normalized}%"))
    })
}

fn lower_like(expression: &str, pattern: String) -> sea_orm::sea_query::SimpleExpr {
    Expr::cust_with_values(format!("LOWER({expression}) LIKE ?"), [pattern])
}

fn normalized_status(value: Option<String>, allowed: &[&str], field: &str) -> KabiPayResult<Option<String>> {
    let Some(raw) = value else {
        return Ok(None);
    };
    let status = raw.trim().to_ascii_uppercase();
    if !allowed.contains(&status.as_str()) {
        return Err(KabiPayError::Validation(format!(
            "invalid {field}; expected {}",
            allowed.join(" | ")
        )));
    }
    Ok(Some(status))
}

fn map_asset_db_error(error: DbErr) -> KabiPayError {
    let message = error.to_string();
    if message.contains("uq_asset_category_tenant_code_normalized_ci")
        || message.contains("uq_asset_category_tenant_code")
    {
        KabiPayError::ConflictRule {
            code: "ASSET_CATEGORY_CODE_CONFLICT",
            message: "asset category code is already in use".into(),
        }
    } else if message.contains("uq_asset_tenant_asset_tag_ci") {
        KabiPayError::ConflictRule {
            code: "ASSET_TAG_CONFLICT",
            message: "asset tag is already in use".into(),
        }
    } else if message.contains("uq_asset_tenant_serial_number_ci") {
        KabiPayError::ConflictRule {
            code: "ASSET_SERIAL_NUMBER_CONFLICT",
            message: "asset serial number is already in use".into(),
        }
    } else if message.contains("uq_asset_allocation_one_active_per_asset") {
        KabiPayError::ConflictRule {
            code: "ASSET_ACTIVE_ALLOCATION_CONFLICT",
            message: "asset already has an active allocation".into(),
        }
    } else if message.contains("uq_asset_return_log_allocation") {
        KabiPayError::ConflictRule {
            code: "ASSET_RETURN_CONFLICT",
            message: "asset allocation has already been returned".into(),
        }
    } else {
        KabiPayError::Database(error)
    }
}

#[cfg(test)]
mod db_error_mapping_tests {
    use super::*;

    #[test]
    fn normalized_category_constraint_has_stable_conflict_code() {
        let error = DbErr::Custom(
            "duplicate key violates uq_asset_category_tenant_code_normalized_ci".into(),
        );
        assert_eq!(map_asset_db_error(error).code(), "ASSET_CATEGORY_CODE_CONFLICT");
    }
}

fn ensure_category_updatable(is_active: bool) -> KabiPayResult<()> {
    if !is_active {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_CATEGORY_STATE_CONFLICT",
            message: "retired asset categories cannot be updated".into(),
        });
    }
    Ok(())
}

fn ensure_category_selectable(is_active: bool) -> KabiPayResult<()> {
    if !is_active {
        return Err(KabiPayError::Validation(
            "select an active asset category".into(),
        ));
    }
    Ok(())
}

fn ensure_asset_editable(status: &str) -> KabiPayResult<()> {
    if status != ASSET_AVAILABLE {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_STATE_CONFLICT",
            message: "only available assets can be edited".into(),
        });
    }
    Ok(())
}

pub async fn list_categories(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<asset_category::Model>> {
    asset_category::Entity::find()
        .filter(asset_category::Column::TenantId.eq(tenant_id))
        .order_by_asc(asset_category::Column::Name)
        .limit(limit.clamp(1, 200))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_category_page(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    page: PageInput,
    search: Option<String>,
    active_only: bool,
) -> KabiPayResult<CategoryPageData> {
    let page = page.clamp();
    let mut query = asset_category::Entity::find()
        .filter(asset_category::Column::TenantId.eq(tenant_id));
    if active_only {
        query = query.filter(asset_category::Column::IsActive.eq(true));
    }
    if let Some(pattern) = search_pattern(search) {
        query = query.filter(
            Condition::any()
                .add(lower_like("name", pattern.clone()))
                .add(lower_like("COALESCE(code, '')", pattern)),
        );
    }
    let total_count = query.clone().count(db).await.map_err(KabiPayError::from)?;
    let rows = query
        .order_by_asc(asset_category::Column::Name)
        .order_by_asc(asset_category::Column::Id)
        .offset(page.offset())
        .limit(page.limit())
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(CategoryPageData {
        rows,
        page_info: PageInfo::compute(page, total_count),
    })
}

pub async fn list_assets(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<asset::Model>> {
    asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .order_by_desc(asset::Column::CreatedAt)
        .limit(limit.clamp(1, 500))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_location_options(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<location::Model>> {
    location::Entity::find()
        .filter(location::Column::TenantId.eq(tenant_id))
        .filter(location::Column::IsDeleted.eq(false))
        .order_by_asc(location::Column::Name)
        .limit(limit.clamp(1, 200))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

fn employee_option_query(tenant_id: Uuid, search: Option<String>) -> Select<employee::Entity> {
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.eq("ACTIVE"));
    if let Some(pattern) = search_pattern(search) {
        query = query.filter(
            Condition::any()
                .add(lower_like("employee_code", pattern.clone()))
                .add(lower_like("TRIM(first_name || ' ' || last_name)", pattern)),
        );
    }
    query
        .order_by_asc(employee::Column::FirstName)
        .order_by_asc(employee::Column::LastName)
        .order_by_asc(employee::Column::EmployeeCode)
        .order_by_asc(employee::Column::Id)
}

fn allocation_employee_search_query(
    tenant_id: Uuid,
    pattern: String,
) -> Select<employee::Entity> {
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(
            Condition::any()
                .add(lower_like("employee_code", pattern.clone()))
                .add(lower_like("first_name || ' ' || last_name", pattern)),
        )
}

pub async fn list_employee_option_page(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    page: PageInput,
    search: Option<String>,
) -> KabiPayResult<EmployeeOptionPageData> {
    let page = page.clamp();
    let query = employee_option_query(tenant_id, search);
    let total_count = query.clone().count(db).await.map_err(KabiPayError::from)?;
    let rows = query
        .offset(page.offset())
        .limit(page.limit())
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(EmployeeOptionPageData {
        rows,
        page_info: PageInfo::compute(page, total_count),
    })
}

pub async fn list_asset_page(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    page: PageInput,
    search: Option<String>,
    category_id: Option<Uuid>,
    status: Option<String>,
) -> KabiPayResult<AssetPageData> {
    let page = page.clamp();
    let status = normalized_status(
        status,
        &[ASSET_AVAILABLE, ASSET_ASSIGNED, ASSET_RETIRED],
        "asset status",
    )?;
    let mut query = asset::Entity::find().filter(asset::Column::TenantId.eq(tenant_id));
    if let Some(category_id) = category_id {
        query = query.filter(asset::Column::AssetCategoryId.eq(category_id));
    }
    if let Some(status) = status {
        query = query.filter(asset::Column::Status.eq(status));
    }
    if let Some(pattern) = search_pattern(search) {
        query = query.filter(
            Condition::any()
                .add(lower_like("name", pattern.clone()))
                .add(lower_like("COALESCE(asset_tag, '')", pattern.clone()))
                .add(lower_like("COALESCE(serial_number, '')", pattern)),
        );
    }
    let total_count = query.clone().count(db).await.map_err(KabiPayError::from)?;
    let assets = query
        .order_by_asc(asset::Column::Name)
        .order_by_asc(asset::Column::Id)
        .offset(page.offset())
        .limit(page.limit())
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let category_ids = assets
        .iter()
        .map(|row| row.asset_category_id)
        .collect::<Vec<_>>();
    let categories = if category_ids.is_empty() {
        Vec::new()
    } else {
        asset_category::Entity::find()
            .filter(asset_category::Column::TenantId.eq(tenant_id))
            .filter(asset_category::Column::Id.is_in(category_ids))
            .all(db)
            .await
            .map_err(KabiPayError::from)?
    };
    let category_map = categories
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let rows = assets
        .into_iter()
        .map(|row| AssetDetail {
            category: category_map.get(&row.asset_category_id).cloned(),
            asset: row,
        })
        .collect();
    Ok(AssetPageData {
        rows,
        page_info: PageInfo::compute(page, total_count),
    })
}

pub async fn list_asset_assignments(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Option<Uuid>,
    active_only: bool,
    limit: u64,
) -> KabiPayResult<Vec<(asset_allocation::Model, asset::Model)>> {
    let mut query = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id));
    if let Some(employee_id) = employee_id {
        query = query.filter(asset_allocation::Column::EmployeeId.eq(employee_id));
    }
    if active_only {
        query = query.filter(asset_allocation::Column::Status.eq(ALLOCATION_ACTIVE));
    }
    let allocations = query
        .order_by_desc(asset_allocation::Column::AllocatedOn)
        .limit(limit.clamp(1, 500))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let details = load_allocation_details(db, tenant_id, allocations).await?;
    Ok(details
        .into_iter()
        .map(|detail| (detail.allocation, detail.asset))
        .collect())
}

pub async fn list_allocation_page(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    page: PageInput,
    search: Option<String>,
    employee_id: Option<Uuid>,
    status: Option<String>,
) -> KabiPayResult<AllocationPageData> {
    let page = page.clamp();
    let status = normalized_status(
        status,
        &[ALLOCATION_ACTIVE, ALLOCATION_RETURNED],
        "allocation status",
    )?;
    let mut query = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id));
    if let Some(employee_id) = employee_id {
        query = query.filter(asset_allocation::Column::EmployeeId.eq(employee_id));
    }
    if let Some(status) = status {
        query = query.filter(asset_allocation::Column::Status.eq(status));
    }
    if let Some(pattern) = search_pattern(search) {
        let asset_ids = asset::Entity::find()
            .select_only()
            .column(asset::Column::Id)
            .filter(asset::Column::TenantId.eq(tenant_id))
            .filter(
                Condition::any()
                    .add(lower_like("name", pattern.clone()))
                    .add(lower_like("COALESCE(asset_tag, '')", pattern.clone()))
                    .add(lower_like("COALESCE(serial_number, '')", pattern.clone())),
            )
            .into_tuple::<Uuid>()
            .all(db)
            .await
            .map_err(KabiPayError::from)?;
        let employee_ids = allocation_employee_search_query(tenant_id, pattern)
            .select_only()
            .column(employee::Column::Id)
            .into_tuple::<Uuid>()
            .all(db)
            .await
            .map_err(KabiPayError::from)?;
        if asset_ids.is_empty() && employee_ids.is_empty() {
            return Ok(AllocationPageData {
                rows: Vec::new(),
                page_info: PageInfo::compute(page, 0),
            });
        }
        let mut match_condition = Condition::any();
        if !asset_ids.is_empty() {
            match_condition = match_condition.add(asset_allocation::Column::AssetId.is_in(asset_ids));
        }
        if !employee_ids.is_empty() {
            match_condition =
                match_condition.add(asset_allocation::Column::EmployeeId.is_in(employee_ids));
        }
        query = query.filter(match_condition);
    }
    let total_count = query.clone().count(db).await.map_err(KabiPayError::from)?;
    let allocations = query
        .order_by_desc(asset_allocation::Column::AllocatedOn)
        .order_by_desc(asset_allocation::Column::CreatedAt)
        .order_by_asc(asset_allocation::Column::Id)
        .offset(page.offset())
        .limit(page.limit())
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(AllocationPageData {
        rows: load_allocation_details(db, tenant_id, allocations).await?,
        page_info: PageInfo::compute(page, total_count),
    })
}

async fn load_allocation_details(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    allocations: Vec<asset_allocation::Model>,
) -> KabiPayResult<Vec<AllocationDetail>> {
    if allocations.is_empty() {
        return Ok(Vec::new());
    }
    let asset_ids = allocations.iter().map(|row| row.asset_id).collect::<Vec<_>>();
    let employee_ids = allocations
        .iter()
        .map(|row| row.employee_id)
        .collect::<Vec<_>>();
    let allocation_ids = allocations.iter().map(|row| row.id).collect::<Vec<_>>();
    let assets = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.is_in(asset_ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let employees = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.is_in(employee_ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let return_logs = asset_return_log::Entity::find()
        .filter(asset_return_log::Column::TenantId.eq(tenant_id))
        .filter(asset_return_log::Column::AssetAllocationId.is_in(allocation_ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    let asset_map = assets.into_iter().map(|row| (row.id, row)).collect::<HashMap<_, _>>();
    let employee_map = employees
        .into_iter()
        .map(|row| (row.id, row))
        .collect::<HashMap<_, _>>();
    let return_map = return_logs
        .into_iter()
        .map(|row| (row.asset_allocation_id, row))
        .collect::<HashMap<_, _>>();
    Ok(allocations
        .into_iter()
        .filter_map(|allocation| {
            asset_map.get(&allocation.asset_id).cloned().map(|asset| AllocationDetail {
                employee: employee_map.get(&allocation.employee_id).cloned(),
                return_log: return_map.get(&allocation.id).cloned(),
                allocation,
                asset,
            })
        })
        .collect())
}

pub async fn upsert_asset_category(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    code: String,
) -> KabiPayResult<asset_category::Model> {
    let name = required_text(&name, "category name")?;
    let code = category_code(&code)?;
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let now = Utc::now();
    let saved = if let Some(id) = id {
        let existing = asset_category::Entity::find()
            .filter(asset_category::Column::TenantId.eq(tenant_id))
            .filter(asset_category::Column::Id.eq(id))
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "asset_category",
                id: id.to_string(),
            })?;
        ensure_category_updatable(existing.is_active)?;
        let mut active: asset_category::ActiveModel = existing.into();
        active.name = Set(name);
        active.code = Set(Some(code));
        active.updated_at = Set(now);
        active.update(&txn).await.map_err(map_asset_db_error)?
    } else {
        asset_category::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            name: Set(name),
            code: Set(Some(code)),
            is_active: Set(true),
            retired_at: Set(None),
            retired_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await
        .map_err(map_asset_db_error)?
    };
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(saved)
}

pub async fn retire_asset_category(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    category_id: Uuid,
    acting_user_id: Uuid,
) -> KabiPayResult<asset_category::Model> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let category = asset_category::Entity::find()
        .filter(asset_category::Column::TenantId.eq(tenant_id))
        .filter(asset_category::Column::Id.eq(category_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset_category",
            id: category_id.to_string(),
        })?;
    if !category.is_active {
        return Ok(category);
    }
    let remaining_assets = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::AssetCategoryId.eq(category_id))
        .filter(asset::Column::Status.ne(ASSET_RETIRED))
        .count(&txn)
        .await
        .map_err(KabiPayError::from)?;
    if remaining_assets > 0 {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_CATEGORY_NOT_EMPTY_CONFLICT",
            message: "retire or move all active assets in this category first".into(),
        });
    }
    let now = Utc::now();
    let mut active: asset_category::ActiveModel = category.into();
    active.is_active = Set(false);
    active.retired_at = Set(Some(now));
    active.retired_by = Set(Some(acting_user_id));
    active.updated_at = Set(now);
    let updated = active.update(&txn).await.map_err(map_asset_db_error)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(updated)
}

#[allow(clippy::too_many_arguments)]
pub async fn upsert_asset(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    category_id: Uuid,
    name: String,
    serial_number: Option<String>,
    asset_tag: Option<String>,
    purchase_value: Option<Decimal>,
    purchase_date: Option<NaiveDate>,
    location_id: Option<Uuid>,
) -> KabiPayResult<asset::Model> {
    let name = required_text(&name, "asset name")?;
    let serial_number = optional_identifier(serial_number);
    let asset_tag = optional_identifier(asset_tag);
    let purchase_value = validate_purchase_value(purchase_value)?;
    let txn = db.begin().await.map_err(KabiPayError::from)?;

    // All writers that can change category lifecycle state lock the category
    // before an asset row. This serializes create/update with category
    // retirement and prevents a new asset from being committed under a
    // category that was retired after validation.
    let category = asset_category::Entity::find()
        .filter(asset_category::Column::TenantId.eq(tenant_id))
        .filter(asset_category::Column::Id.eq(category_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?;
    ensure_category_selectable(category.is_some_and(|row| row.is_active))?;
    if let Some(location_id) = location_id {
        let location = location::Entity::find()
            .filter(location::Column::TenantId.eq(tenant_id))
            .filter(location::Column::Id.eq(location_id))
            .lock_shared()
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?;
        if !location.is_some_and(|row| !row.is_deleted) {
            return Err(KabiPayError::Validation(
                "select an active tenant location".into(),
            ));
        }
    }
    let now = Utc::now();
    let saved = if let Some(id) = id {
        let existing = asset::Entity::find()
            .filter(asset::Column::TenantId.eq(tenant_id))
            .filter(asset::Column::Id.eq(id))
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "asset",
                id: id.to_string(),
            })?;
        ensure_asset_editable(&existing.status)?;
        let mut active: asset::ActiveModel = existing.into();
        active.asset_category_id = Set(category_id);
        active.name = Set(name);
        active.serial_number = Set(serial_number);
        active.asset_tag = Set(asset_tag);
        active.purchase_value = Set(purchase_value);
        active.purchase_date = Set(purchase_date);
        active.location_id = Set(location_id);
        active.updated_at = Set(now);
        active.update(&txn).await.map_err(map_asset_db_error)?
    } else {
        asset::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            asset_category_id: Set(category_id),
            name: Set(name),
            serial_number: Set(serial_number),
            asset_tag: Set(asset_tag),
            purchase_value: Set(purchase_value),
            purchase_date: Set(purchase_date),
            status: Set(ASSET_AVAILABLE.to_string()),
            location_id: Set(location_id),
            retired_at: Set(None),
            retired_by: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(&txn)
        .await
        .map_err(map_asset_db_error)?
    };
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(saved)
}

pub async fn retire_asset(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    asset_id: Uuid,
    acting_user_id: Uuid,
) -> KabiPayResult<asset::Model> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let asset_row = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.eq(asset_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset",
            id: asset_id.to_string(),
        })?;
    if asset_row.status == ASSET_RETIRED {
        return Ok(asset_row);
    }
    if asset_row.status != ASSET_AVAILABLE {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_STATE_CONFLICT",
            message: "return the assigned asset before retiring it".into(),
        });
    }
    let active_allocations = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id))
        .filter(asset_allocation::Column::AssetId.eq(asset_id))
        .filter(asset_allocation::Column::Status.eq(ALLOCATION_ACTIVE))
        .count(&txn)
        .await
        .map_err(KabiPayError::from)?;
    if active_allocations > 0 {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_ACTIVE_ALLOCATION_CONFLICT",
            message: "return the assigned asset before retiring it".into(),
        });
    }
    let now = Utc::now();
    let mut active: asset::ActiveModel = asset_row.into();
    active.status = Set(ASSET_RETIRED.to_string());
    active.retired_at = Set(Some(now));
    active.retired_by = Set(Some(acting_user_id));
    active.updated_at = Set(now);
    let updated = active.update(&txn).await.map_err(map_asset_db_error)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(updated)
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
    if expected_return_on.is_some_and(|expected| expected < allocated_on) {
        return Err(KabiPayError::Validation(
            "expectedReturnOn cannot be before allocatedOn".into(),
        ));
    }
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let asset_row = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.eq(asset_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset",
            id: asset_id.to_string(),
        })?;
    if asset_row.status != ASSET_AVAILABLE {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_STATE_CONFLICT",
            message: "only available assets can be assigned".into(),
        });
    }
    let employee_exists = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::Id.eq(employee_id))
        .lock_shared()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .is_some_and(|row| !row.is_deleted && row.status == "ACTIVE");
    if !employee_exists {
        return Err(KabiPayError::Validation(
            "select an active employee".into(),
        ));
    }
    let now = Utc::now();
    let allocation = asset_allocation::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        asset_id: Set(asset_id),
        employee_id: Set(employee_id),
        allocated_on: Set(allocated_on),
        expected_return_on: Set(expected_return_on),
        condition_at_allocation: Set(optional_identifier(condition_at_allocation)),
        status: Set(ALLOCATION_ACTIVE.to_string()),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(map_asset_db_error)?;
    let mut active_asset: asset::ActiveModel = asset_row.into();
    active_asset.status = Set(ASSET_ASSIGNED.to_string());
    active_asset.updated_at = Set(now);
    let updated_asset = active_asset
        .update(&txn)
        .await
        .map_err(map_asset_db_error)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok((allocation, updated_asset))
}

#[allow(clippy::too_many_arguments)]
pub async fn return_asset(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    allocation_id: Uuid,
    returned_on: NaiveDate,
    condition_at_return: Option<String>,
    remarks: Option<String>,
    received_by: Uuid,
) -> KabiPayResult<(asset_allocation::Model, asset::Model)> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let allocation = asset_allocation::Entity::find()
        .filter(asset_allocation::Column::TenantId.eq(tenant_id))
        .filter(asset_allocation::Column::Id.eq(allocation_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset_allocation",
            id: allocation_id.to_string(),
        })?;
    if allocation.status != ALLOCATION_ACTIVE {
        return Err(KabiPayError::ConflictRule {
            code: "ASSET_RETURN_CONFLICT",
            message: "asset allocation has already been returned".into(),
        });
    }
    if returned_on < allocation.allocated_on {
        return Err(KabiPayError::Validation(
            "returnedOn cannot be before allocatedOn".into(),
        ));
    }
    let asset_row = asset::Entity::find()
        .filter(asset::Column::TenantId.eq(tenant_id))
        .filter(asset::Column::Id.eq(allocation.asset_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "asset",
            id: allocation.asset_id.to_string(),
        })?;
    let now = Utc::now();
    asset_return_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        asset_allocation_id: Set(allocation.id),
        returned_on: Set(returned_on),
        condition_at_return: Set(optional_identifier(condition_at_return)),
        remarks: Set(optional_identifier(remarks)),
        received_by: Set(Some(received_by)),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await
    .map_err(map_asset_db_error)?;
    let mut active_allocation: asset_allocation::ActiveModel = allocation.into();
    active_allocation.status = Set(ALLOCATION_RETURNED.to_string());
    active_allocation.updated_at = Set(now);
    let updated_allocation = active_allocation
        .update(&txn)
        .await
        .map_err(map_asset_db_error)?;
    let mut active_asset: asset::ActiveModel = asset_row.into();
    active_asset.status = Set(ASSET_AVAILABLE.to_string());
    active_asset.updated_at = Set(now);
    let updated_asset = active_asset
        .update(&txn)
        .await
        .map_err(map_asset_db_error)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok((updated_allocation, updated_asset))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, QueryTrait};

    #[test]
    fn asset_status_filter_is_case_normalized() {
        assert_eq!(
            normalized_status(Some(" available ".into()), &[ASSET_AVAILABLE], "asset status")
                .unwrap(),
            Some(ASSET_AVAILABLE.into())
        );
    }

    #[test]
    fn asset_status_filter_rejects_unknown_values() {
        assert!(normalized_status(Some("lost".into()), &[ASSET_AVAILABLE], "asset status").is_err());
    }

    #[test]
    fn blank_search_is_not_applied() {
        assert_eq!(search_pattern(Some("   ".into())), None);
        assert_eq!(search_pattern(Some(" Lap ".into())), Some("%lap%".into()));
    }

    #[test]
    fn asset_conflicts_have_stable_codes_without_database_details() {
        let cases = [
            (
                "uq_asset_category_tenant_code",
                "ASSET_CATEGORY_CODE_CONFLICT",
            ),
            ("uq_asset_tenant_asset_tag_ci", "ASSET_TAG_CONFLICT"),
            (
                "uq_asset_tenant_serial_number_ci",
                "ASSET_SERIAL_NUMBER_CONFLICT",
            ),
            (
                "uq_asset_allocation_one_active_per_asset",
                "ASSET_ACTIVE_ALLOCATION_CONFLICT",
            ),
            ("uq_asset_return_log_allocation", "ASSET_RETURN_CONFLICT"),
        ];

        for (constraint, expected_code) in cases {
            let error = map_asset_db_error(DbErr::Custom(format!(
                "duplicate key violates {constraint}; private row detail"
            )));
            assert_eq!(error.code(), expected_code);
            let graphql = error.into_graphql();
            assert!(!graphql.message.contains(constraint));
            assert!(!graphql.message.contains("private row detail"));
        }
    }

    #[test]
    fn lifecycle_edit_rechecks_assignment_state_after_lock() {
        let error = ensure_asset_editable(ASSET_ASSIGNED).unwrap_err();
        assert_eq!(error.code(), "ASSET_STATE_CONFLICT");
    }

    #[test]
    fn lifecycle_edit_rechecks_retirement_state_after_lock() {
        let error = ensure_asset_editable(ASSET_RETIRED).unwrap_err();
        assert_eq!(error.code(), "ASSET_STATE_CONFLICT");
    }

    #[test]
    fn category_update_rechecks_retirement_state_after_lock() {
        let error = ensure_category_updatable(false).unwrap_err();
        assert_eq!(error.code(), "ASSET_CATEGORY_STATE_CONFLICT");
    }

    #[test]
    fn asset_create_rechecks_category_state_after_lock() {
        let error = ensure_category_selectable(false).unwrap_err();
        assert_eq!(error.code(), "VALIDATION_ERROR");
    }

    #[test]
    fn employee_option_query_is_active_tenant_scoped_and_deterministic() {
        let tenant_id = Uuid::parse_str("e6d4fc13-feb8-52a0-93bd-f66c795969b1").unwrap();
        let statement = employee_option_query(tenant_id, Some(" Ada ".into()))
            .build(DbBackend::Postgres)
            .to_string();

        assert!(statement.contains("\"tenant_id\" = 'e6d4fc13-feb8-52a0-93bd-f66c795969b1'"));
        assert!(statement.contains("\"status\" = 'ACTIVE'"));
        assert!(statement.contains("\"is_deleted\" = FALSE"));
        assert!(statement.contains("ORDER BY \"employee\".\"first_name\" ASC, \"employee\".\"last_name\" ASC, \"employee\".\"employee_code\" ASC, \"employee\".\"id\" ASC"));
        assert!(statement.contains("%ada%"));
    }

    #[test]
    fn allocation_history_employee_search_includes_deleted_employee_records() {
        let tenant_id = Uuid::parse_str("e6d4fc13-feb8-52a0-93bd-f66c795969b1").unwrap();
        let statement = allocation_employee_search_query(tenant_id, "%ada%".into())
            .build(DbBackend::Postgres)
            .to_string();

        assert!(statement.contains("\"tenant_id\" = 'e6d4fc13-feb8-52a0-93bd-f66c795969b1'"));
        assert!(!statement.contains("\"is_deleted\" = FALSE"));
        assert!(statement.contains("%ada%"));
    }
}
