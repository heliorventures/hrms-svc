use chrono::{NaiveDate, Utc};
use kabipay_common::{
    working_calendar::{bump_calendar_revision, lock_calendar},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0010_time_shift_roster::{holiday, holiday_calendar};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter,
    Set, TransactionTrait,
};
use uuid::Uuid;

pub async fn upsert_holiday_calendar(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    year: i32,
    location_id: Option<Uuid>,
) -> KabiPayResult<holiday_calendar::Model> {
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant_id).await?;
    let row = upsert_holiday_calendar_in_txn(&txn, tenant_id, id, name, year, location_id).await?;
    bump_calendar_revision(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(row)
}
pub async fn delete_holiday_calendar(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    calendar_id: Uuid,
) -> KabiPayResult<u64> {
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant_id).await?;
    let result = delete_holiday_calendar_in_txn(&txn, tenant_id, calendar_id).await?;
    bump_calendar_revision(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(result)
}
pub async fn upsert_holiday_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    calendar_id: Uuid,
    id: Option<Uuid>,
    holiday_date: NaiveDate,
    name: String,
    holiday_type: Option<String>,
) -> KabiPayResult<holiday::Model> {
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant_id).await?;
    let row = upsert_holiday_entry_in_txn(
        &txn,
        tenant_id,
        calendar_id,
        id,
        holiday_date,
        name,
        holiday_type,
    )
    .await?;
    bump_calendar_revision(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(row)
}
pub async fn delete_holiday_entry(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    holiday_id: Uuid,
) -> KabiPayResult<u64> {
    let txn = db.begin().await?;
    lock_calendar(&txn, tenant_id).await?;
    let result = delete_holiday_entry_in_txn(&txn, tenant_id, holiday_id).await?;
    bump_calendar_revision(&txn, tenant_id).await?;
    txn.commit().await?;
    Ok(result)
}

async fn upsert_holiday_calendar_in_txn<C: ConnectionTrait + Sync>(
    db: &C,
    tenant_id: Uuid,
    id: Option<Uuid>,
    name: String,
    year: i32,
    location_id: Option<Uuid>,
) -> KabiPayResult<holiday_calendar::Model> {
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(KabiPayError::Validation(
            "holiday calendar name is required".into(),
        ));
    }
    if let Some(location_id) = location_id {
        use kabipay_db_entities::tenant::d0006_org_hierarchy::location;
        if location::Entity::find_by_id(location_id)
            .filter(location::Column::TenantId.eq(tenant_id))
            .filter(location::Column::IsDeleted.eq(false))
            .one(db)
            .await?
            .is_none()
        {
            return Err(KabiPayError::Validation(
                "select an active location in this company".into(),
            ));
        }
    }
    let now = Utc::now();
    if let Some(cid) = id {
        let row = holiday_calendar::Entity::find_by_id(cid)
            .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "holiday_calendar",
                id: cid.to_string(),
            })?;
        let mut am: holiday_calendar::ActiveModel = row.into();
        am.name = Set(name);
        am.year = Set(year);
        am.location_id = Set(location_id);
        am.updated_at = Set(now);
        return Ok(am.update(db).await?);
    }
    let new_id = Uuid::new_v4();
    let am = holiday_calendar::ActiveModel {
        id: Set(new_id),
        tenant_id: Set(tenant_id),
        location_id: Set(location_id),
        name: Set(name),
        year: Set(year),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    holiday_calendar::Entity::find_by_id(new_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted holiday_calendar not found".into()))
}

async fn delete_holiday_calendar_in_txn<C: ConnectionTrait + Sync>(
    db: &C,
    tenant_id: Uuid,
    calendar_id: Uuid,
) -> KabiPayResult<u64> {
    let n = holiday_calendar::Entity::delete_many()
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .filter(holiday_calendar::Column::Id.eq(calendar_id))
        .exec(db)
        .await?
        .rows_affected;
    Ok(n)
}

pub(super) async fn assert_calendar_tenant<C: ConnectionTrait>(
    db: &C,
    tenant_id: Uuid,
    calendar_id: Uuid,
) -> KabiPayResult<holiday_calendar::Model> {
    holiday_calendar::Entity::find_by_id(calendar_id)
        .filter(holiday_calendar::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "holiday_calendar",
            id: calendar_id.to_string(),
        })
}

async fn upsert_holiday_entry_in_txn<C: ConnectionTrait + Sync>(
    db: &C,
    tenant_id: Uuid,
    calendar_id: Uuid,
    id: Option<Uuid>,
    holiday_date: NaiveDate,
    name: String,
    holiday_type: Option<String>,
) -> KabiPayResult<holiday::Model> {
    assert_calendar_tenant(db, tenant_id, calendar_id).await?;
    let name = name.trim().to_string();
    if name.is_empty() {
        return Err(KabiPayError::Validation("holiday name is required".into()));
    }
    let now = Utc::now();
    if let Some(hid) = id {
        let row = holiday::Entity::find_by_id(hid)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "holiday",
                id: hid.to_string(),
            })?;
        if row.calendar_id != calendar_id {
            return Err(KabiPayError::Validation(
                "holiday does not belong to this calendar".into(),
            ));
        }
        let mut am: holiday::ActiveModel = row.into();
        am.holiday_date = Set(holiday_date);
        am.name = Set(name);
        am.r#type = Set(holiday_type);
        am.updated_at = Set(now);
        return Ok(am.update(db).await?);
    }
    let new_id = Uuid::new_v4();
    let am = holiday::ActiveModel {
        id: Set(new_id),
        calendar_id: Set(calendar_id),
        holiday_date: Set(holiday_date),
        name: Set(name),
        r#type: Set(holiday_type),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await?;
    holiday::Entity::find_by_id(new_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted holiday not found".into()))
}

async fn delete_holiday_entry_in_txn<C: ConnectionTrait + Sync>(
    db: &C,
    tenant_id: Uuid,
    holiday_id: Uuid,
) -> KabiPayResult<u64> {
    let row = holiday::Entity::find_by_id(holiday_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "holiday",
            id: holiday_id.to_string(),
        })?;
    assert_calendar_tenant(db, tenant_id, row.calendar_id).await?;
    let n = holiday::Entity::delete_many()
        .filter(holiday::Column::Id.eq(holiday_id))
        .exec(db)
        .await?
        .rows_affected;
    Ok(n)
}
