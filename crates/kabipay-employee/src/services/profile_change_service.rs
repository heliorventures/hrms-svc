//! Audited change requests for legal identity, statutory identity, and bank details.

use chrono::{NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::{
    employee, employee_aadhaar, employee_bank, employee_pan,
};
use kabipay_db_entities::tenant::d0008_document_system::employee_document;
use kabipay_db_entities::tenant::d0050_employee_self_service::employee_profile_change_request;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const REQUEST_LEGAL: &str = "LEGAL_NAME_OR_DOB";
pub const REQUEST_PAN: &str = "PAN";
pub const REQUEST_AADHAAR: &str = "AADHAAR";
pub const REQUEST_BANK: &str = "BANK_ACCOUNT";

#[derive(Clone, Debug)]
pub struct NewProfileChange {
    pub request_type: String,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub date_of_birth: Option<NaiveDate>,
    pub pan_number: Option<String>,
    pub aadhaar_number: Option<String>,
    pub bank_name: Option<String>,
    pub account_number: Option<String>,
    pub ifsc_code: Option<String>,
    pub account_type: Option<String>,
    pub supporting_document_id: Option<Uuid>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct LegalPayload {
    first_name: Option<String>,
    last_name: Option<String>,
    date_of_birth: Option<NaiveDate>,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct PanPayload {
    pan_number: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct AadhaarPayload {
    aadhaar_last4: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
struct BankPayload {
    bank_name: String,
    account_number: String,
    ifsc_code: String,
    account_type: Option<String>,
}

fn optional_trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn required(value: Option<String>, field: &str) -> KabiPayResult<String> {
    optional_trimmed(value)
        .ok_or_else(|| KabiPayError::Validation(format!("{field} is required")))
}

fn normalize_pan(value: Option<String>) -> KabiPayResult<String> {
    let pan = required(value, "panNumber")?.to_uppercase().replace(' ', "");
    let bytes = pan.as_bytes();
    let valid = bytes.len() == 10
        && bytes[..5].iter().all(u8::is_ascii_uppercase)
        && bytes[5..9].iter().all(u8::is_ascii_digit)
        && bytes[9].is_ascii_uppercase();
    if !valid {
        return Err(KabiPayError::Validation(
            "PAN must match AAAAA9999A".into(),
        ));
    }
    Ok(pan)
}

fn normalize_aadhaar_last4(value: Option<String>) -> KabiPayResult<String> {
    let digits: String = required(value, "aadhaarNumber")?
        .chars()
        .filter(char::is_ascii_digit)
        .collect();
    match digits.len() {
        4 => Ok(digits),
        12 => Ok(digits[8..].to_string()),
        _ => Err(KabiPayError::Validation(
            "Aadhaar must contain 12 digits or the last 4 digits".into(),
        )),
    }
}

fn validated_payload(input: NewProfileChange) -> KabiPayResult<(String, serde_json::Value, Option<Uuid>)> {
    let request_type = input.request_type.trim().to_uppercase();
    let payload = match request_type.as_str() {
        REQUEST_LEGAL => {
            let first_name = optional_trimmed(input.first_name);
            let last_name = optional_trimmed(input.last_name);
            if first_name.is_none() && last_name.is_none() && input.date_of_birth.is_none() {
                return Err(KabiPayError::Validation(
                    "legal profile request must change a name or date of birth".into(),
                ));
            }
            serde_json::to_value(LegalPayload {
                first_name,
                last_name,
                date_of_birth: input.date_of_birth,
            })?
        }
        REQUEST_PAN => serde_json::to_value(PanPayload {
            pan_number: normalize_pan(input.pan_number)?,
        })?,
        REQUEST_AADHAAR => serde_json::to_value(AadhaarPayload {
            aadhaar_last4: normalize_aadhaar_last4(input.aadhaar_number)?,
        })?,
        REQUEST_BANK => serde_json::to_value(BankPayload {
            bank_name: required(input.bank_name, "bankName")?,
            account_number: required(input.account_number, "accountNumber")?,
            ifsc_code: required(input.ifsc_code, "ifscCode")?.to_uppercase(),
            account_type: optional_trimmed(input.account_type),
        })?,
        _ => {
            return Err(KabiPayError::Validation(
                "requestType must be LEGAL_NAME_OR_DOB, PAN, AADHAAR, or BANK_ACCOUNT".into(),
            ))
        }
    };
    Ok((request_type, payload, input.supporting_document_id))
}

pub async fn submit_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    requested_by: Uuid,
    input: NewProfileChange,
) -> KabiPayResult<employee_profile_change_request::Model> {
    let (request_type, requested_payload, supporting_document_id) = validated_payload(input)?;

    if let Some(document_id) = supporting_document_id {
        let owns_document = employee_document::Entity::find_by_id(document_id)
            .filter(employee_document::Column::TenantId.eq(tenant_id))
            .filter(employee_document::Column::EmployeeId.eq(employee_id))
            .filter(employee_document::Column::IsDeleted.eq(false))
            .one(db)
            .await?
            .is_some();
        if !owns_document {
            return Err(KabiPayError::Validation(
                "supporting document does not belong to this employee".into(),
            ));
        }
    }

    let pending = employee_profile_change_request::Entity::find()
        .filter(employee_profile_change_request::Column::TenantId.eq(tenant_id))
        .filter(employee_profile_change_request::Column::EmployeeId.eq(employee_id))
        .filter(employee_profile_change_request::Column::RequestType.eq(request_type.clone()))
        .filter(employee_profile_change_request::Column::Status.eq("PENDING"))
        .one(db)
        .await?;
    if pending.is_some() {
        return Err(KabiPayError::Conflict(format!(
            "a pending {request_type} change already exists"
        )));
    }

    let now = Utc::now();
    let id = Uuid::new_v4();
    employee_profile_change_request::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        requested_by: Set(requested_by),
        request_type: Set(request_type),
        requested_payload: Set(requested_payload),
        status: Set("PENDING".into()),
        supporting_document_id: Set(supporting_document_id),
        reviewed_by: Set(None),
        reviewed_at: Set(None),
        rejection_reason: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?;

    employee_profile_change_request::Entity::find_by_id(id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("inserted profile change request missing".into()))
}

pub async fn list_requests(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    status: Option<&str>,
) -> KabiPayResult<Vec<employee_profile_change_request::Model>> {
    let mut query = employee_profile_change_request::Entity::find()
        .filter(employee_profile_change_request::Column::TenantId.eq(tenant_id))
        .filter(employee_profile_change_request::Column::EmployeeId.eq(employee_id));
    if let Some(status) = status.filter(|value| !value.trim().is_empty()) {
        query = query.filter(
            employee_profile_change_request::Column::Status.eq(status.trim().to_uppercase()),
        );
    }
    query
        .order_by_desc(employee_profile_change_request::Column::CreatedAt)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn find_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
) -> KabiPayResult<Option<employee_profile_change_request::Model>> {
    employee_profile_change_request::Entity::find_by_id(request_id)
        .filter(employee_profile_change_request::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn cancel_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    actor_user_id: Uuid,
    can_manage: bool,
) -> KabiPayResult<employee_profile_change_request::Model> {
    let row = employee_profile_change_request::Entity::find_by_id(request_id)
        .filter(employee_profile_change_request::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeProfileChangeRequest",
            id: request_id.to_string(),
        })?;
    if row.status != "PENDING" {
        return Err(KabiPayError::Conflict(
            "only pending profile changes can be cancelled".into(),
        ));
    }
    if row.requested_by != actor_user_id && !can_manage {
        return Err(KabiPayError::Forbidden(
            "only the requester or HR may cancel this change".into(),
        ));
    }
    let mut active: employee_profile_change_request::ActiveModel = row.into();
    active.status = Set("CANCELLED".into());
    active.updated_at = Set(Utc::now());
    active.update(db).await.map_err(KabiPayError::from)
}

pub async fn resolve_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    reviewer_user_id: Uuid,
    approved: bool,
    rejection_reason: Option<String>,
) -> KabiPayResult<employee_profile_change_request::Model> {
    let txn = db.begin().await?;
    let row = employee_profile_change_request::Entity::find_by_id(request_id)
        .filter(employee_profile_change_request::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeProfileChangeRequest",
            id: request_id.to_string(),
        })?;
    if row.status != "PENDING" {
        return Err(KabiPayError::Conflict(
            "only pending profile changes can be reviewed".into(),
        ));
    }
    if row.requested_by == reviewer_user_id {
        return Err(KabiPayError::Forbidden(
            "requesters cannot approve or reject their own profile change".into(),
        ));
    }
    if !approved && optional_trimmed(rejection_reason.clone()).is_none() {
        return Err(KabiPayError::Validation(
            "rejectionReason is required when rejecting a profile change".into(),
        ));
    }

    if approved {
        apply_approved_request(&txn, &row).await?;
    }

    let now = Utc::now();
    if let Some(document_id) = row.supporting_document_id {
        if let Some(document) = employee_document::Entity::find_by_id(document_id)
            .filter(employee_document::Column::TenantId.eq(tenant_id))
            .filter(employee_document::Column::EmployeeId.eq(row.employee_id))
            .filter(employee_document::Column::IsDeleted.eq(false))
            .one(&txn)
            .await?
        {
            let mut document_active: employee_document::ActiveModel = document.into();
            document_active.status = Set(if approved { "APPROVED" } else { "REJECTED" }.into());
            document_active.verified_by = Set(Some(reviewer_user_id));
            document_active.verified_at = Set(Some(now));
            document_active.updated_at = Set(now);
            document_active.update(&txn).await?;
        }
    }
    let mut active: employee_profile_change_request::ActiveModel = row.into();
    active.status = Set(if approved { "APPROVED" } else { "REJECTED" }.into());
    active.reviewed_by = Set(Some(reviewer_user_id));
    active.reviewed_at = Set(Some(now));
    active.rejection_reason = Set(if approved {
        None
    } else {
        optional_trimmed(rejection_reason)
    });
    active.updated_at = Set(now);
    active.update(&txn).await?;
    txn.commit().await?;

    employee_profile_change_request::Entity::find_by_id(request_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("reviewed profile change request missing".into()))
}

async fn apply_approved_request(
    txn: &sea_orm::DatabaseTransaction,
    request: &employee_profile_change_request::Model,
) -> KabiPayResult<()> {
    match request.request_type.as_str() {
        REQUEST_LEGAL => {
            let payload: LegalPayload = serde_json::from_value(request.requested_payload.clone())?;
            let row = employee::Entity::find_by_id(request.employee_id)
                .filter(employee::Column::TenantId.eq(request.tenant_id))
                .filter(employee::Column::IsDeleted.eq(false))
                .one(txn)
                .await?
                .ok_or_else(|| KabiPayError::NotFound {
                    entity: "employee",
                    id: request.employee_id.to_string(),
                })?;
            let mut active: employee::ActiveModel = row.into();
            if let Some(value) = payload.first_name {
                active.first_name = Set(value);
            }
            if let Some(value) = payload.last_name {
                active.last_name = Set(value);
            }
            if let Some(value) = payload.date_of_birth {
                active.date_of_birth = Set(Some(value));
            }
            active.updated_at = Set(Utc::now());
            active.update(txn).await?;
        }
        REQUEST_PAN => {
            let payload: PanPayload = serde_json::from_value(request.requested_payload.clone())?;
            apply_primary_pan(txn, request.tenant_id, request.employee_id, payload.pan_number)
                .await?;
        }
        REQUEST_AADHAAR => {
            let payload: AadhaarPayload = serde_json::from_value(request.requested_payload.clone())?;
            apply_primary_aadhaar(
                txn,
                request.tenant_id,
                request.employee_id,
                payload.aadhaar_last4,
            )
            .await?;
        }
        REQUEST_BANK => {
            let payload: BankPayload = serde_json::from_value(request.requested_payload.clone())?;
            apply_primary_bank(txn, request.tenant_id, request.employee_id, payload).await?;
        }
        _ => {
            return Err(KabiPayError::Validation(
                "unsupported profile change request type".into(),
            ))
        }
    }
    Ok(())
}

async fn apply_primary_pan(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    employee_id: Uuid,
    pan_number: String,
) -> KabiPayResult<()> {
    let rows = employee_pan::Entity::find()
        .filter(employee_pan::Column::TenantId.eq(tenant_id))
        .filter(employee_pan::Column::EmployeeId.eq(employee_id))
        .all(txn)
        .await?;
    let target_id = rows.iter().find(|row| row.is_primary).map(|row| row.id);
    let now = Utc::now();
    if rows.is_empty() {
        employee_pan::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            pan_number: Set(pan_number),
            is_primary: Set(true),
            is_verified: Set(false),
            verified_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
        return Ok(());
    }
    let target_id = target_id.unwrap_or(rows[0].id);
    for row in rows {
        let is_target = row.id == target_id;
        let mut active: employee_pan::ActiveModel = row.into();
        active.is_primary = Set(is_target);
        if is_target {
            active.pan_number = Set(pan_number.clone());
            active.is_verified = Set(false);
            active.verified_at = Set(None);
        }
        active.updated_at = Set(now);
        active.update(txn).await?;
    }
    Ok(())
}

async fn apply_primary_aadhaar(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    employee_id: Uuid,
    aadhaar_last4: String,
) -> KabiPayResult<()> {
    let rows = employee_aadhaar::Entity::find()
        .filter(employee_aadhaar::Column::TenantId.eq(tenant_id))
        .filter(employee_aadhaar::Column::EmployeeId.eq(employee_id))
        .all(txn)
        .await?;
    let target_id = rows.iter().find(|row| row.is_primary).map(|row| row.id);
    let now = Utc::now();
    if rows.is_empty() {
        employee_aadhaar::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            aadhaar_last4: Set(aadhaar_last4),
            is_primary: Set(true),
            is_verified: Set(false),
            verified_at: Set(None),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
        return Ok(());
    }
    let target_id = target_id.unwrap_or(rows[0].id);
    for row in rows {
        let is_target = row.id == target_id;
        let mut active: employee_aadhaar::ActiveModel = row.into();
        active.is_primary = Set(is_target);
        if is_target {
            active.aadhaar_last4 = Set(aadhaar_last4.clone());
            active.is_verified = Set(false);
            active.verified_at = Set(None);
        }
        active.updated_at = Set(now);
        active.update(txn).await?;
    }
    Ok(())
}

async fn apply_primary_bank(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    employee_id: Uuid,
    payload: BankPayload,
) -> KabiPayResult<()> {
    let rows = employee_bank::Entity::find()
        .filter(employee_bank::Column::TenantId.eq(tenant_id))
        .filter(employee_bank::Column::EmployeeId.eq(employee_id))
        .all(txn)
        .await?;
    let target_id = rows.iter().find(|row| row.is_primary).map(|row| row.id);
    let now = Utc::now();
    if rows.is_empty() {
        employee_bank::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            account_number: Set(payload.account_number),
            ifsc_code: Set(payload.ifsc_code),
            bank_name: Set(payload.bank_name),
            account_type: Set(payload.account_type),
            is_primary: Set(true),
            is_verified: Set(false),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(txn)
        .await?;
        return Ok(());
    }
    let target_id = target_id.unwrap_or(rows[0].id);
    for row in rows {
        let is_target = row.id == target_id;
        let mut active: employee_bank::ActiveModel = row.into();
        active.is_primary = Set(is_target);
        if is_target {
            active.account_number = Set(payload.account_number.clone());
            active.ifsc_code = Set(payload.ifsc_code.clone());
            active.bank_name = Set(payload.bank_name.clone());
            active.account_type = Set(payload.account_type.clone());
            active.is_verified = Set(false);
        }
        active.updated_at = Set(now);
        active.update(txn).await?;
    }
    Ok(())
}
