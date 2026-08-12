//! Safe tenant-wide employee directory reads.
//!
//! This service intentionally returns employee models only to the resolver that
//! maps an allow-listed public projection. It does not consume private employee
//! resource scopes.

use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct DirectoryPage {
    pub rows: Vec<employee::Model>,
    pub next_cursor: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct DirectoryCursor {
    employee_code: String,
    id: Uuid,
}

fn encode_cursor(row: &employee::Model) -> KabiPayResult<String> {
    let bytes = serde_json::to_vec(&DirectoryCursor {
        employee_code: row.employee_code.clone(),
        id: row.id,
    })?;
    Ok(URL_SAFE_NO_PAD.encode(bytes))
}

fn decode_cursor(raw: &str) -> KabiPayResult<DirectoryCursor> {
    let bytes = URL_SAFE_NO_PAD
        .decode(raw.trim())
        .map_err(|_| KabiPayError::Validation("invalid employee directory cursor".into()))?;
    serde_json::from_slice(&bytes)
        .map_err(|_| KabiPayError::Validation("invalid employee directory cursor".into()))
}

fn current_employee_query(tenant_id: Uuid) -> sea_orm::Select<employee::Entity> {
    employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.ne("TERMINATED"))
}

pub async fn list_page(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    after: Option<&str>,
) -> KabiPayResult<DirectoryPage> {
    let limit = limit.clamp(1, 100);
    let mut query = current_employee_query(tenant_id);
    if let Some(raw) = after.filter(|value| !value.trim().is_empty()) {
        let cursor = decode_cursor(raw)?;
        query = query.filter(
            Condition::any()
                .add(employee::Column::EmployeeCode.gt(cursor.employee_code.clone()))
                .add(
                    Condition::all()
                        .add(employee::Column::EmployeeCode.eq(cursor.employee_code))
                        .add(employee::Column::Id.gt(cursor.id)),
                ),
        );
    }

    let mut rows = query
        .order_by_asc(employee::Column::EmployeeCode)
        .order_by_asc(employee::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .map_err(KabiPayError::from)?;

    let has_more = rows.len() as u64 > limit;
    if has_more {
        rows.pop();
    }
    let next_cursor = if has_more {
        rows.last().map(encode_cursor).transpose()?
    } else {
        None
    };

    Ok(DirectoryPage { rows, next_cursor })
}

pub async fn list_hierarchy(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<employee::Model>> {
    current_employee_query(tenant_id)
        .order_by_asc(employee::Column::EmployeeCode)
        .order_by_asc(employee::Column::Id)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn find_current_by_id(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Option<employee::Model>> {
    current_employee_query(tenant_id)
        .filter(employee::Column::Id.eq(employee_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, Utc};

    fn employee_row() -> employee::Model {
        employee::Model {
            id: Uuid::parse_str("00000000-0000-0000-0000-000000000002").unwrap(),
            tenant_id: Uuid::nil(),
            user_id: None,
            department_id: None,
            designation_id: None,
            cost_center_id: None,
            location_id: None,
            reporting_manager_id: None,
            employee_code: "EMP002".into(),
            first_name: "Directory".into(),
            last_name: "User".into(),
            date_of_birth: None,
            gender: None,
            blood_group: None,
            nationality: None,
            employment_type: None,
            status: "ACTIVE".into(),
            date_of_joining: NaiveDate::from_ymd_opt(2026, 1, 1).unwrap(),
            probation_end_date: None,
            notice_period_days: None,
            emergency_contact_name: None,
            emergency_contact_phone: None,
            emergency_contact_relation: None,
            personal_phone: None,
            current_address: None,
            permanent_address: None,
            uan_number: None,
            esic_number: None,
            is_deleted: false,
            deleted_at: None,
            deleted_by: None,
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[test]
    fn directory_cursor_round_trips_stable_ordering_keys() {
        let row = employee_row();
        let encoded = encode_cursor(&row).unwrap();
        let decoded = decode_cursor(&encoded).unwrap();
        assert_eq!(decoded.employee_code, row.employee_code);
        assert_eq!(decoded.id, row.id);
    }
}
