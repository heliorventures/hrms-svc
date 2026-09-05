//! Audited change requests for legal identity, statutory identity, and bank details.

use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0007_employee_core::{
    employee, employee_aadhaar, employee_bank, employee_pan,
};
use kabipay_db_entities::tenant::d0008_document_system::employee_document;
use kabipay_db_entities::tenant::d0005_auth_rbac::{permission, role_permission, user, user_role};
use kabipay_db_entities::tenant::d0027_communication_audit::{audit_log, notification};
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use kabipay_db_entities::tenant::d0050_employee_self_service::employee_profile_change_request;
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, ConnectionTrait, DatabaseConnection,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

use super::profile_payload_crypto::{
    PayloadContext, ProfilePayloadCipher, PAYLOAD_ENCRYPTION_VERSION,
};

pub const REQUEST_LEGAL: &str = "LEGAL_NAME_OR_DOB";
pub const REQUEST_PAN: &str = "PAN";
pub const REQUEST_AADHAAR: &str = "AADHAAR";
pub const REQUEST_BANK: &str = "BANK_ACCOUNT";
const OUTBOX_PENDING: &str = "PENDING";

async fn insert_audit_and_outbox<C: ConnectionTrait>(
    conn: &C,
    tenant_id: Uuid,
    actor_user_id: Uuid,
    request_id: Uuid,
    employee_id: Uuid,
    request_type: &str,
    action: &str,
    before_status: Option<&str>,
    after_status: &str,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    audit_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(Some(actor_user_id)),
        entity_type: Set("employee_profile_change_request".into()),
        entity_id: Set(Some(request_id)),
        action: Set(action.into()),
        before_state: Set(before_status.map(|status| serde_json::json!({"status": status}))),
        after_state: Set(Some(serde_json::json!({
            "status": after_status,
            "requestType": request_type,
            "employeeId": employee_id,
        }))),
        ip_address: Set(None),
        user_agent: Set(None),
        created_at: Set(now),
    }
    .insert(conn)
    .await?;

    outbox_event::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        aggregate_type: Set("employee_profile_change_request".into()),
        aggregate_id: Set(request_id),
        event_type: Set(format!("employee_profile_change_request.{action}")),
        payload: Set(serde_json::json!({
            "schema_version": 1,
            "request_id": request_id,
            "employee_id": employee_id,
            "request_type": request_type,
            "status": after_status,
            "actor_user_id": actor_user_id,
        })),
        status: Set(OUTBOX_PENDING.into()),
        retry_count: Set(0),
        last_error: Set(None),
        created_at: Set(now),
        processed_at: Set(None),
        claimed_at: Set(None),
    }
    .insert(conn)
    .await?;
    Ok(())
}

async fn insert_notification<C: ConnectionTrait>(
    conn: &C,
    tenant_id: Uuid,
    user_id: Uuid,
    kind: &str,
    title: &str,
    message: String,
    action_url: String,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    notification::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        r#type: Set(Some(kind.into())),
        title: Set(Some(title.into())),
        message: Set(Some(message)),
        action_url: Set(Some(action_url)),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(conn)
    .await?;
    Ok(())
}

async fn hr_reviewer_user_ids<C: ConnectionTrait>(
    conn: &C,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    let permission_ids = permission::Entity::find()
        .filter(permission::Column::Resource.eq("employee"))
        .filter(permission::Column::Action.is_in(["write", "manage"]))
        .all(conn)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect::<Vec<_>>();
    if permission_ids.is_empty() {
        return Ok(Vec::new());
    }
    let role_ids = role_permission::Entity::find()
        .filter(role_permission::Column::PermissionId.is_in(permission_ids))
        .all(conn)
        .await?
        .into_iter()
        .map(|row| row.role_id)
        .collect::<Vec<_>>();
    if role_ids.is_empty() {
        return Ok(Vec::new());
    }
    let user_ids = user_role::Entity::find()
        .filter(user_role::Column::RoleId.is_in(role_ids))
        .all(conn)
        .await?
        .into_iter()
        .map(|row| row.user_id)
        .collect::<std::collections::HashSet<_>>();
    if user_ids.is_empty() {
        return Ok(Vec::new());
    }
    Ok(user::Entity::find()
        .filter(user::Column::TenantId.eq(tenant_id))
        .filter(user::Column::Id.is_in(user_ids))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::IsDeleted.eq(false))
        .all(conn)
        .await?
        .into_iter()
        .map(|row| row.id)
        .collect())
}

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

#[derive(Clone, Copy, Debug, PartialEq)]
struct IdentityVerificationStamp {
    is_verified: bool,
    verified_at: Option<chrono::DateTime<Utc>>,
}

fn approved_identity_stamp(reviewed_at: chrono::DateTime<Utc>) -> IdentityVerificationStamp {
    IdentityVerificationStamp {
        is_verified: true,
        verified_at: Some(reviewed_at),
    }
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

fn required_limited(value: Option<String>, field: &str, max: usize) -> KabiPayResult<String> {
    let value = required(value, field)?;
    if value.chars().count() > max {
        return Err(KabiPayError::Validation(format!(
            "{field} must not exceed {max} characters"
        )));
    }
    Ok(value)
}

fn optional_limited(value: Option<String>, field: &str, max: usize) -> KabiPayResult<Option<String>> {
    let value = optional_trimmed(value);
    if value.as_ref().is_some_and(|value| value.chars().count() > max) {
        return Err(KabiPayError::Validation(format!(
            "{field} must not exceed {max} characters"
        )));
    }
    Ok(value)
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

fn validate_date_of_birth(value: NaiveDate) -> KabiPayResult<NaiveDate> {
    let today = Utc::now().date_naive();
    if value >= today {
        return Err(KabiPayError::Validation(
            "dateOfBirth must be earlier than today".into(),
        ));
    }
    if value.year() < today.year() - 120 {
        return Err(KabiPayError::Validation(
            "dateOfBirth must be within the last 120 years".into(),
        ));
    }
    Ok(value)
}

fn normalize_ifsc(value: Option<String>) -> KabiPayResult<String> {
    let ifsc = required_limited(value, "ifscCode", 11)?.to_uppercase();
    let bytes = ifsc.as_bytes();
    let valid = bytes.len() == 11
        && bytes[..4].iter().all(u8::is_ascii_uppercase)
        && bytes[4] == b'0'
        && bytes[5..].iter().all(u8::is_ascii_alphanumeric);
    if !valid {
        return Err(KabiPayError::Validation(
            "IFSC must match AAAA0XXXXXX".into(),
        ));
    }
    Ok(ifsc)
}

fn normalize_account_number(value: Option<String>) -> KabiPayResult<String> {
    let account = required_limited(value, "accountNumber", 34)?.replace([' ', '-'], "");
    if !(6..=34).contains(&account.len()) || !account.chars().all(|c| c.is_ascii_digit()) {
        return Err(KabiPayError::Validation(
            "accountNumber must contain 6 to 34 digits".into(),
        ));
    }
    Ok(account)
}

fn normalize_account_type(value: Option<String>) -> KabiPayResult<Option<String>> {
    let value = optional_limited(value, "accountType", 30)?.map(|v| v.to_uppercase());
    if let Some(value) = value.as_deref() {
        if !matches!(value, "SAVINGS" | "CURRENT" | "SALARY" | "NRE" | "NRO" | "OTHER") {
            return Err(KabiPayError::Validation(
                "accountType must be SAVINGS, CURRENT, SALARY, NRE, NRO, or OTHER".into(),
            ));
        }
    }
    Ok(value)
}

fn summary_last4(value: &str) -> String {
    value
        .chars()
        .rev()
        .take(4)
        .collect::<String>()
        .chars()
        .rev()
        .collect()
}

fn pan_summary_digits(value: &str) -> String {
    value.chars().skip(5).take(4).collect()
}

fn validated_payload(
    input: NewProfileChange,
) -> KabiPayResult<(String, serde_json::Value, serde_json::Value, Option<Uuid>)> {
    let request_type = input.request_type.trim().to_uppercase();
    let (payload, summary) = match request_type.as_str() {
        REQUEST_LEGAL => {
            let first_name = optional_limited(input.first_name, "firstName", 100)?;
            let last_name = optional_limited(input.last_name, "lastName", 100)?;
            let date_of_birth = input.date_of_birth.map(validate_date_of_birth).transpose()?;
            if first_name.is_none() && last_name.is_none() && date_of_birth.is_none() {
                return Err(KabiPayError::Validation(
                    "legal profile request must change a name or date of birth".into(),
                ));
            }
            let changed_fields = [
                first_name.as_ref().map(|_| "firstName"),
                last_name.as_ref().map(|_| "lastName"),
                date_of_birth.as_ref().map(|_| "dateOfBirth"),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>();
            (serde_json::to_value(LegalPayload {
                first_name,
                last_name,
                date_of_birth,
            })?, serde_json::json!({"changedFields": changed_fields}))
        }
        REQUEST_PAN => {
            let pan_number = normalize_pan(input.pan_number)?;
            let last4 = pan_summary_digits(&pan_number);
            (serde_json::to_value(PanPayload { pan_number })?, serde_json::json!({"last4": last4}))
        }
        REQUEST_AADHAAR => {
            let aadhaar_last4 = normalize_aadhaar_last4(input.aadhaar_number)?;
            (serde_json::to_value(AadhaarPayload { aadhaar_last4: aadhaar_last4.clone() })?, serde_json::json!({"last4": aadhaar_last4}))
        }
        REQUEST_BANK => {
            let account_number = normalize_account_number(input.account_number)?;
            let last4 = summary_last4(&account_number);
            (serde_json::to_value(BankPayload {
                bank_name: required_limited(input.bank_name, "bankName", 150)?,
                account_number,
                ifsc_code: normalize_ifsc(input.ifsc_code)?,
                account_type: normalize_account_type(input.account_type)?,
            })?, serde_json::json!({"last4": last4}))
        }
        _ => {
            return Err(KabiPayError::Validation(
                "requestType must be LEGAL_NAME_OR_DOB, PAN, AADHAAR, or BANK_ACCOUNT".into(),
            ))
        }
    };
    Ok((request_type, payload, summary, input.supporting_document_id))
}

fn validate_stored_payload(request_type: &str, payload: serde_json::Value) -> KabiPayResult<serde_json::Value> {
    let input = match request_type {
        REQUEST_LEGAL => {
            let payload: LegalPayload = serde_json::from_value(payload)?;
            NewProfileChange {
                request_type: request_type.into(),
                first_name: payload.first_name,
                last_name: payload.last_name,
                date_of_birth: payload.date_of_birth,
                pan_number: None,
                aadhaar_number: None,
                bank_name: None,
                account_number: None,
                ifsc_code: None,
                account_type: None,
                supporting_document_id: None,
            }
        }
        REQUEST_PAN => {
            let payload: PanPayload = serde_json::from_value(payload)?;
            NewProfileChange { request_type: request_type.into(), pan_number: Some(payload.pan_number), first_name: None, last_name: None, date_of_birth: None, aadhaar_number: None, bank_name: None, account_number: None, ifsc_code: None, account_type: None, supporting_document_id: None }
        }
        REQUEST_AADHAAR => {
            let payload: AadhaarPayload = serde_json::from_value(payload)?;
            NewProfileChange { request_type: request_type.into(), aadhaar_number: Some(payload.aadhaar_last4), first_name: None, last_name: None, date_of_birth: None, pan_number: None, bank_name: None, account_number: None, ifsc_code: None, account_type: None, supporting_document_id: None }
        }
        REQUEST_BANK => {
            let payload: BankPayload = serde_json::from_value(payload)?;
            NewProfileChange { request_type: request_type.into(), bank_name: Some(payload.bank_name), account_number: Some(payload.account_number), ifsc_code: Some(payload.ifsc_code), account_type: payload.account_type, first_name: None, last_name: None, date_of_birth: None, pan_number: None, aadhaar_number: None, supporting_document_id: None }
        }
        _ => return Err(KabiPayError::Validation("unsupported profile change request type".into())),
    };
    let (_, payload, _, _) = validated_payload(input)?;
    Ok(payload)
}

fn decrypt_request_payload(request: &employee_profile_change_request::Model) -> KabiPayResult<serde_json::Value> {
    let payload = if let Some(encrypted) = request.requested_payload_encrypted.as_deref() {
        if request.payload_encryption_version != Some(PAYLOAD_ENCRYPTION_VERSION) {
            return Err(KabiPayError::Internal(
                "unsupported profile payload encryption version".into(),
            ));
        }
        ProfilePayloadCipher::from_env()?.decrypt(
            &PayloadContext {
                tenant_id: request.tenant_id,
                request_id: request.id,
                request_type: &request.request_type,
            },
            encrypted,
        )?
    } else {
        // Backward-compatible read for pending rows created before migration 0051.
        request.requested_payload.clone()
    };
    validate_stored_payload(&request.request_type, payload)
}

fn masked_summary(request_type: &str, payload: &serde_json::Value) -> serde_json::Value {
    match request_type {
        REQUEST_LEGAL => {
            let changed_fields = [
                payload.get("first_name").filter(|value| !value.is_null()).map(|_| "firstName"),
                payload.get("last_name").filter(|value| !value.is_null()).map(|_| "lastName"),
                payload.get("date_of_birth").filter(|value| !value.is_null()).map(|_| "dateOfBirth"),
            ].into_iter().flatten().collect::<Vec<_>>();
            serde_json::json!({"changedFields": changed_fields})
        }
        REQUEST_PAN => serde_json::json!({"last4": payload.get("pan_number").and_then(serde_json::Value::as_str).map(pan_summary_digits)}),
        REQUEST_AADHAAR => serde_json::json!({"last4": payload.get("aadhaar_last4").and_then(serde_json::Value::as_str)}),
        REQUEST_BANK => serde_json::json!({"last4": payload.get("account_number").and_then(serde_json::Value::as_str).map(summary_last4)}),
        _ => serde_json::json!({}),
    }
}

pub async fn submit_request(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    requested_by: Uuid,
    input: NewProfileChange,
) -> KabiPayResult<employee_profile_change_request::Model> {
    let (request_type, sensitive_payload, requested_payload, supporting_document_id) =
        validated_payload(input)?;

    let txn = db.begin().await?;
    if let Some(document_id) = supporting_document_id {
        let owns_document = employee_document::Entity::find_by_id(document_id)
            .filter(employee_document::Column::TenantId.eq(tenant_id))
            .filter(employee_document::Column::EmployeeId.eq(employee_id))
            .filter(employee_document::Column::IsDeleted.eq(false))
            .one(&txn)
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
        .filter(employee_profile_change_request::Column::RequestType.eq(request_type.as_str()))
        .filter(employee_profile_change_request::Column::Status.eq("PENDING"))
        .one(&txn)
        .await?;
    if pending.is_some() {
        return Err(KabiPayError::Conflict(format!(
            "a pending {request_type} change already exists"
        )));
    }

    let now = Utc::now();
    let id = Uuid::new_v4();
    let requested_payload_encrypted = ProfilePayloadCipher::from_env()?.encrypt(
        &PayloadContext {
            tenant_id,
            request_id: id,
            request_type: &request_type,
        },
        &sensitive_payload,
    )?;
    employee_profile_change_request::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        requested_by: Set(requested_by),
        request_type: Set(request_type.clone()),
        requested_payload: Set(requested_payload),
        requested_payload_encrypted: Set(Some(requested_payload_encrypted)),
        payload_encryption_version: Set(Some(PAYLOAD_ENCRYPTION_VERSION)),
        status: Set("PENDING".into()),
        supporting_document_id: Set(supporting_document_id),
        reviewed_by: Set(None),
        reviewed_at: Set(None),
        rejection_reason: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(&txn)
    .await?;

    insert_audit_and_outbox(
        &txn,
        tenant_id,
        requested_by,
        id,
        employee_id,
        &request_type,
        "submitted",
        None,
        "PENDING",
        now,
    )
    .await?;
    for reviewer_id in hr_reviewer_user_ids(&txn, tenant_id).await? {
        if reviewer_id != requested_by {
            insert_notification(
                &txn,
                tenant_id,
                reviewer_id,
                "EMPLOYEE_PROFILE_REVIEW",
                "Employee profile change requires review",
                format!("A {request_type} profile change is waiting for review."),
                "/organization/profile-reviews".into(),
                now,
            )
            .await?;
        }
    }
    txn.commit().await?;

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

pub async fn list_review_queue(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    status: Option<&str>,
    limit: u64,
    employee_ids: Option<&[Uuid]>,
) -> KabiPayResult<Vec<employee_profile_change_request::Model>> {
    let mut query = employee_profile_change_request::Entity::find()
        .filter(employee_profile_change_request::Column::TenantId.eq(tenant_id));
    if let Some(employee_ids) = employee_ids {
        if employee_ids.is_empty() { return Ok(Vec::new()); }
        query = query.filter(employee_profile_change_request::Column::EmployeeId.is_in(employee_ids.iter().copied()));
    }
    if let Some(status) = status.filter(|value| !value.trim().is_empty()) {
        let status = status.trim().to_uppercase();
        if !matches!(status.as_str(), "PENDING" | "APPROVED" | "REJECTED" | "CANCELLED") {
            return Err(KabiPayError::Validation(
                "status must be PENDING, APPROVED, REJECTED, or CANCELLED".into(),
            ));
        }
        query = query.filter(employee_profile_change_request::Column::Status.eq(status));
    }
    query
        .order_by_desc(employee_profile_change_request::Column::CreatedAt)
        .order_by_desc(employee_profile_change_request::Column::Id)
        .limit(limit.clamp(1, 200))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn review_values(
    db: &DatabaseConnection,
    request: &employee_profile_change_request::Model,
) -> KabiPayResult<(serde_json::Value, serde_json::Value)> {
    let requested = decrypt_request_payload(request)?;
    let current = match request.request_type.as_str() {
        REQUEST_LEGAL => {
            let row = employee::Entity::find_by_id(request.employee_id)
                .filter(employee::Column::TenantId.eq(request.tenant_id))
                .filter(employee::Column::IsDeleted.eq(false))
                .one(db)
                .await?
                .ok_or_else(|| KabiPayError::NotFound {
                    entity: "employee",
                    id: request.employee_id.to_string(),
                })?;
            serde_json::json!({
                "first_name": row.first_name,
                "last_name": row.last_name,
                "date_of_birth": row.date_of_birth,
            })
        }
        REQUEST_PAN => {
            let row = employee_pan::Entity::find()
                .filter(employee_pan::Column::TenantId.eq(request.tenant_id))
                .filter(employee_pan::Column::EmployeeId.eq(request.employee_id))
                .filter(employee_pan::Column::IsPrimary.eq(true))
                .one(db)
                .await?;
            serde_json::json!({"pan_number": row.map(|value| value.pan_number)})
        }
        REQUEST_AADHAAR => {
            let row = employee_aadhaar::Entity::find()
                .filter(employee_aadhaar::Column::TenantId.eq(request.tenant_id))
                .filter(employee_aadhaar::Column::EmployeeId.eq(request.employee_id))
                .filter(employee_aadhaar::Column::IsPrimary.eq(true))
                .one(db)
                .await?;
            serde_json::json!({"aadhaar_last4": row.map(|value| value.aadhaar_last4)})
        }
        REQUEST_BANK => {
            let row = employee_bank::Entity::find()
                .filter(employee_bank::Column::TenantId.eq(request.tenant_id))
                .filter(employee_bank::Column::EmployeeId.eq(request.employee_id))
                .filter(employee_bank::Column::IsPrimary.eq(true))
                .one(db)
                .await?;
            match row {
                Some(value) => serde_json::json!({
                    "bank_name": value.bank_name,
                    "account_number": value.account_number,
                    "ifsc_code": value.ifsc_code,
                    "account_type": value.account_type,
                }),
                None => serde_json::json!({
                    "bank_name": null,
                    "account_number": null,
                    "ifsc_code": null,
                    "account_type": null,
                }),
            }
        }
        _ => return Err(KabiPayError::Validation("unsupported profile change request type".into())),
    };
    Ok((current, requested))
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
            "only pending profile changes can be cancelled".into(),
        ));
    }
    if row.requested_by != actor_user_id && !can_manage {
        return Err(KabiPayError::Forbidden(
            "only the requester or HR may cancel this change".into(),
        ));
    }
    let sensitive_payload = decrypt_request_payload(&row)?;
    let summary = masked_summary(&row.request_type, &sensitive_payload);
    let request_type = row.request_type.clone();
    let employee_id = row.employee_id;
    let mut active: employee_profile_change_request::ActiveModel = row.into();
    active.status = Set("CANCELLED".into());
    active.requested_payload = Set(summary);
    active.requested_payload_encrypted = Set(None);
    active.payload_encryption_version = Set(None);
    let now = Utc::now();
    active.updated_at = Set(now);
    active.update(&txn).await?;
    insert_audit_and_outbox(
        &txn, tenant_id, actor_user_id, request_id, employee_id, &request_type,
        "cancelled", Some("PENDING"), "CANCELLED", now,
    ).await?;
    txn.commit().await?;
    employee_profile_change_request::Entity::find_by_id(request_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("cancelled profile change request missing".into()))
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

    let requested_payload = decrypt_request_payload(&row)?;
    let summary = masked_summary(&row.request_type, &requested_payload);
    let now = Utc::now();
    if approved {
        apply_approved_request(&txn, &row, requested_payload, now).await?;
    }

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
    let requester_user_id = row.requested_by;
    let employee_id = row.employee_id;
    let request_type = row.request_type.clone();
    let mut active: employee_profile_change_request::ActiveModel = row.into();
    active.status = Set(if approved { "APPROVED" } else { "REJECTED" }.into());
    active.requested_payload = Set(summary);
    active.requested_payload_encrypted = Set(None);
    active.payload_encryption_version = Set(None);
    active.reviewed_by = Set(Some(reviewer_user_id));
    active.reviewed_at = Set(Some(now));
    active.rejection_reason = Set(if approved {
        None
    } else {
        optional_trimmed(rejection_reason)
    });
    active.updated_at = Set(now);
    active.update(&txn).await?;
    let status = if approved { "APPROVED" } else { "REJECTED" };
    let action = if approved { "approved" } else { "rejected" };
    insert_audit_and_outbox(
        &txn,
        tenant_id,
        reviewer_user_id,
        request_id,
        employee_id,
        &request_type,
        action,
        Some("PENDING"),
        status,
        now,
    )
    .await?;
    insert_notification(
        &txn,
        tenant_id,
        requester_user_id,
        "EMPLOYEE_PROFILE_CHANGE",
        if approved { "Profile change approved" } else { "Profile change rejected" },
        format!("Your {request_type} profile change was {}.", status.to_lowercase()),
        format!("/organization/employees/{employee_id}"),
        now,
    )
    .await?;
    txn.commit().await?;

    employee_profile_change_request::Entity::find_by_id(request_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("reviewed profile change request missing".into()))
}

async fn apply_approved_request(
    txn: &sea_orm::DatabaseTransaction,
    request: &employee_profile_change_request::Model,
    requested_payload: serde_json::Value,
    reviewed_at: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    match request.request_type.as_str() {
        REQUEST_LEGAL => {
            let payload: LegalPayload = serde_json::from_value(requested_payload)?;
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
            let payload: PanPayload = serde_json::from_value(requested_payload)?;
            apply_primary_pan(
                txn,
                request.tenant_id,
                request.employee_id,
                payload.pan_number,
                reviewed_at,
            )
            .await?;
        }
        REQUEST_AADHAAR => {
            let payload: AadhaarPayload = serde_json::from_value(requested_payload)?;
            apply_primary_aadhaar(
                txn,
                request.tenant_id,
                request.employee_id,
                payload.aadhaar_last4,
                reviewed_at,
            )
            .await?;
        }
        REQUEST_BANK => {
            let payload: BankPayload = serde_json::from_value(requested_payload)?;
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
    reviewed_at: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let rows = employee_pan::Entity::find()
        .filter(employee_pan::Column::TenantId.eq(tenant_id))
        .filter(employee_pan::Column::EmployeeId.eq(employee_id))
        .all(txn)
        .await?;
    let target_id = rows.iter().find(|row| row.is_primary).map(|row| row.id);
    let now = Utc::now();
    let verification = approved_identity_stamp(reviewed_at);
    if rows.is_empty() {
        employee_pan::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            pan_number: Set(pan_number),
            is_primary: Set(true),
            is_verified: Set(verification.is_verified),
            verified_at: Set(verification.verified_at),
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
            active.is_verified = Set(verification.is_verified);
            active.verified_at = Set(verification.verified_at);
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
    reviewed_at: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let rows = employee_aadhaar::Entity::find()
        .filter(employee_aadhaar::Column::TenantId.eq(tenant_id))
        .filter(employee_aadhaar::Column::EmployeeId.eq(employee_id))
        .all(txn)
        .await?;
    let target_id = rows.iter().find(|row| row.is_primary).map(|row| row.id);
    let now = Utc::now();
    let verification = approved_identity_stamp(reviewed_at);
    if rows.is_empty() {
        employee_aadhaar::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            aadhaar_last4: Set(aadhaar_last4),
            is_primary: Set(true),
            is_verified: Set(verification.is_verified),
            verified_at: Set(verification.verified_at),
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
            active.is_verified = Set(verification.is_verified);
            active.verified_at = Set(verification.verified_at);
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
            is_verified: Set(true),
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
            active.is_verified = Set(true);
        }
        active.updated_at = Set(now);
        active.update(txn).await?;
    }
    Ok(())
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    fn change(request_type: &str) -> NewProfileChange {
        NewProfileChange {
            request_type: request_type.into(),
            first_name: None,
            last_name: None,
            date_of_birth: None,
            pan_number: None,
            aadhaar_number: None,
            bank_name: None,
            account_number: None,
            ifsc_code: None,
            account_type: None,
            supporting_document_id: None,
        }
    }

    #[test]
    fn rejects_future_date_of_birth() {
        let mut input = change(REQUEST_LEGAL);
        input.date_of_birth = Some(Utc::now().date_naive() + chrono::Days::new(1));
        assert!(validated_payload(input).is_err());
    }

    #[test]
    fn rejects_invalid_ifsc_and_account_number() {
        let mut input = change(REQUEST_BANK);
        input.bank_name = Some("Example Bank".into());
        input.account_number = Some("12A".into());
        input.ifsc_code = Some("BAD".into());
        assert!(validated_payload(input).is_err());
    }

    #[test]
    fn produces_only_masked_summary_metadata() {
        let mut input = change(REQUEST_PAN);
        input.pan_number = Some("ABCDE1234F".into());
        let (_, _, summary, _) = validated_payload(input).expect("valid PAN");
        assert_eq!(summary, serde_json::json!({"last4": "1234"}));
        assert!(!summary.to_string().contains("ABCDE"));
    }

    #[test]
    fn approved_identity_stamp_records_hr_review_verification() {
        let reviewed_at = Utc::now();
        let stamp = approved_identity_stamp(reviewed_at);
        assert!(stamp.is_verified);
        assert_eq!(stamp.verified_at, Some(reviewed_at));
    }
}
