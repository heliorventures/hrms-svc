//! Persistent employee education and prior-work records with auditable evidence links.

use chrono::{Datelike, NaiveDate, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait,
    ConnectionTrait, QueryFilter, QueryOrder, QuerySelect, TransactionTrait,
};
use uuid::Uuid;
use std::collections::HashMap;

use crate::entities::d0008_document_system::employee_document;
use crate::entities::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0027_communication_audit::{audit_log, notification};
use kabipay_db_entities::tenant::d0030_outbox_events::outbox_event;
use crate::entities::d0050_employee_self_service::{
    employee_education, employee_education_document, employee_work_experience,
    employee_work_experience_document,
};

const EDUCATION_LEVELS: [&str; 8] = [
    "SECONDARY",
    "HIGHER_SECONDARY",
    "DIPLOMA",
    "UNDERGRADUATE",
    "POSTGRADUATE",
    "DOCTORATE",
    "CERTIFICATION",
    "OTHER",
];

async fn record_review_decision<C: ConnectionTrait>(
    conn: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Uuid,
    reviewer_user_id: Uuid,
    entity_type: &str,
    approved: bool,
    now: chrono::DateTime<Utc>,
) -> KabiPayResult<()> {
    let status = if approved { "VERIFIED" } else { "REJECTED" };
    let action = if approved { "approved" } else { "rejected" };
    audit_log::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(Some(reviewer_user_id)),
        entity_type: Set(entity_type.into()),
        entity_id: Set(Some(record_id)),
        action: Set(action.into()),
        before_state: Set(Some(serde_json::json!({"verificationStatus": "PENDING"}))),
        after_state: Set(Some(serde_json::json!({"verificationStatus": status, "employeeId": employee_id}))),
        ip_address: Set(None),
        user_agent: Set(None),
        created_at: Set(now),
    }.insert(conn).await?;
    outbox_event::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        aggregate_type: Set(entity_type.into()),
        aggregate_id: Set(record_id),
        event_type: Set(format!("{entity_type}.{action}")),
        payload: Set(serde_json::json!({"schema_version": 1, "record_id": record_id, "employee_id": employee_id, "status": status, "reviewer_user_id": reviewer_user_id})),
        status: Set("PENDING".into()),
        retry_count: Set(0),
        last_error: Set(None),
        created_at: Set(now),
        processed_at: Set(None),
        claimed_at: Set(None),
    }.insert(conn).await?;
    if let Some(user_id) = employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(conn).await?.and_then(|row| row.user_id)
    {
        notification::ActiveModel {
            id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), user_id: Set(user_id),
            r#type: Set(Some("EMPLOYEE_PROFILE_EVIDENCE".into())),
            title: Set(Some(format!("{} evidence {}", if entity_type == "employee_education" { "Education" } else { "Work experience" }, action))),
            message: Set(Some(format!("Your profile evidence was {action}."))),
            action_url: Set(Some(format!("/organization/employees/{employee_id}"))),
            is_read: Set(false), read_at: Set(None), created_at: Set(now), updated_at: Set(now),
        }.insert(conn).await?;
    }
    Ok(())
}

pub struct EducationRecordInput {
    pub education_level: String,
    pub qualification: String,
    pub field_of_study: Option<String>,
    pub institution: String,
    pub board_university: Option<String>,
    pub start_date: Option<NaiveDate>,
    pub completion_year: i32,
    pub grade_score: Option<String>,
    pub description: Option<String>,
}

pub struct WorkExperienceRecordInput {
    pub company: String,
    pub role_title: String,
    pub employment_type: Option<String>,
    pub location: Option<String>,
    pub start_date: NaiveDate,
    pub end_date: Option<NaiveDate>,
    pub is_current: bool,
    pub description: Option<String>,
}

pub enum PendingEvidenceRecord {
    Education(employee_education::Model),
    Work(employee_work_experience::Model),
}

impl PendingEvidenceRecord {
    pub fn record_id(&self) -> Uuid {
        match self { Self::Education(row) => row.id, Self::Work(row) => row.id }
    }
    pub fn employee_id(&self) -> Uuid {
        match self { Self::Education(row) => row.employee_id, Self::Work(row) => row.employee_id }
    }
    pub fn created_at(&self) -> chrono::DateTime<Utc> {
        match self { Self::Education(row) => row.created_at, Self::Work(row) => row.created_at }
    }
}

pub async fn list_pending_evidence_reviews(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
    employee_ids: Option<&[Uuid]>,
) -> KabiPayResult<Vec<PendingEvidenceRecord>> {
    if employee_ids.is_some_and(|ids| ids.is_empty()) { return Ok(Vec::new()); }
    let per_type = limit.clamp(1, 100);
    let mut education_query = employee_education::Entity::find()
        .filter(employee_education::Column::TenantId.eq(tenant_id))
        .filter(employee_education::Column::VerificationStatus.eq("PENDING"))
        .filter(employee_education::Column::IsDeleted.eq(false));
    if let Some(employee_ids) = employee_ids {
        education_query = education_query.filter(employee_education::Column::EmployeeId.is_in(employee_ids.iter().copied()));
    }
    let education = education_query
        .order_by_desc(employee_education::Column::CreatedAt)
        .limit(per_type)
        .all(db).await?;
    let mut work_query = employee_work_experience::Entity::find()
        .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience::Column::VerificationStatus.eq("PENDING"))
        .filter(employee_work_experience::Column::IsDeleted.eq(false));
    if let Some(employee_ids) = employee_ids {
        work_query = work_query.filter(employee_work_experience::Column::EmployeeId.is_in(employee_ids.iter().copied()));
    }
    let work = work_query
        .order_by_desc(employee_work_experience::Column::CreatedAt)
        .limit(per_type)
        .all(db).await?;
    let mut rows = education.into_iter().map(PendingEvidenceRecord::Education)
        .chain(work.into_iter().map(PendingEvidenceRecord::Work)).collect::<Vec<_>>();
    rows.sort_by_key(|row| std::cmp::Reverse(row.created_at()));
    rows.truncate(limit.clamp(1, 100) as usize);
    Ok(rows)
}

fn optional_trimmed(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let trimmed = raw.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    })
}

fn optional_limited(value: Option<String>, field: &str, max_len: usize) -> KabiPayResult<Option<String>> {
    let value = optional_trimmed(value);
    if value.as_ref().is_some_and(|value| value.chars().count() > max_len) {
        return Err(KabiPayError::Validation(format!(
            "{field} must be at most {max_len} characters"
        )));
    }
    Ok(value)
}

fn required(value: String, field: &str, max_len: usize) -> KabiPayResult<String> {
    let value = value.trim();
    if value.is_empty() {
        return Err(KabiPayError::Validation(format!("{field} is required")));
    }
    if value.chars().count() > max_len {
        return Err(KabiPayError::Validation(format!(
            "{field} must be at most {max_len} characters"
        )));
    }
    Ok(value.to_string())
}

fn validate_education(mut input: EducationRecordInput) -> KabiPayResult<EducationRecordInput> {
    input.education_level = input.education_level.trim().to_uppercase();
    if !EDUCATION_LEVELS.contains(&input.education_level.as_str()) {
        return Err(KabiPayError::Validation(format!(
            "educationLevel must be one of {}",
            EDUCATION_LEVELS.join(", ")
        )));
    }
    input.qualification = required(input.qualification, "qualification", 255)?;
    input.institution = required(input.institution, "institution", 255)?;
    input.field_of_study = optional_limited(input.field_of_study, "fieldOfStudy", 255)?;
    input.board_university = optional_limited(input.board_university, "boardUniversity", 255)?;
    input.grade_score = optional_limited(input.grade_score, "gradeScore", 100)?;
    input.description = optional_limited(input.description, "description", 4_000)?;

    let max_year = Utc::now().year();
    if !(1900..=max_year).contains(&input.completion_year) {
        return Err(KabiPayError::Validation(format!(
            "completionYear must be between 1900 and {max_year}"
        )));
    }
    if let Some(start_date) = input.start_date {
        if start_date > Utc::now().date_naive() {
            return Err(KabiPayError::Validation(
                "startDate cannot be in the future".into(),
            ));
        }
        if start_date.year() > input.completion_year {
            return Err(KabiPayError::Validation(
                "startDate cannot be after completionYear".into(),
            ));
        }
    }
    Ok(input)
}

fn validate_work(
    mut input: WorkExperienceRecordInput,
) -> KabiPayResult<WorkExperienceRecordInput> {
    input.company = required(input.company, "company", 255)?;
    input.role_title = required(input.role_title, "roleTitle", 255)?;
    input.employment_type = optional_limited(input.employment_type, "employmentType", 50)?;
    input.location = optional_limited(input.location, "location", 255)?;
    input.description = optional_limited(input.description, "description", 4_000)?;
    let today = Utc::now().date_naive();
    if input.start_date > today {
        return Err(KabiPayError::Validation(
            "startDate cannot be in the future".into(),
        ));
    }
    if input.is_current && input.end_date.is_some() {
        return Err(KabiPayError::Validation(
            "endDate must be empty for a current role".into(),
        ));
    }
    if !input.is_current && input.end_date.is_none() {
        return Err(KabiPayError::Validation(
            "endDate is required for a previous role".into(),
        ));
    }
    if let Some(end_date) = input.end_date {
        if end_date > today {
            return Err(KabiPayError::Validation(
                "endDate cannot be in the future".into(),
            ));
        }
        if end_date < input.start_date {
            return Err(KabiPayError::Validation(
                "endDate cannot be before startDate".into(),
            ));
        }
    }
    Ok(input)
}

pub async fn list_education(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Vec<employee_education::Model>> {
    employee_education::Entity::find()
        .filter(employee_education::Column::TenantId.eq(tenant_id))
        .filter(employee_education::Column::EmployeeId.eq(employee_id))
        .filter(employee_education::Column::IsDeleted.eq(false))
        .order_by_desc(employee_education::Column::CompletionYear)
        .order_by_asc(employee_education::Column::Institution)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn find_education(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_id: Uuid,
) -> KabiPayResult<Option<employee_education::Model>> {
    employee_education::Entity::find_by_id(record_id)
        .filter(employee_education::Column::TenantId.eq(tenant_id))
        .filter(employee_education::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn save_education(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Option<Uuid>,
    input: EducationRecordInput,
) -> KabiPayResult<employee_education::Model> {
    let input = validate_education(input)?;
    let now = Utc::now();
    let id = record_id.unwrap_or_else(Uuid::new_v4);
    if let Some(record_id) = record_id {
        let existing = employee_education::Entity::find_by_id(record_id)
            .filter(employee_education::Column::TenantId.eq(tenant_id))
            .filter(employee_education::Column::EmployeeId.eq(employee_id))
            .filter(employee_education::Column::IsDeleted.eq(false))
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "employeeEducation",
                id: record_id.to_string(),
            })?;
        let mut active: employee_education::ActiveModel = existing.into();
        active.education_level = Set(input.education_level);
        active.qualification = Set(input.qualification);
        active.field_of_study = Set(input.field_of_study);
        active.institution = Set(input.institution);
        active.board_university = Set(input.board_university);
        active.start_date = Set(input.start_date);
        active.completion_year = Set(input.completion_year);
        active.grade_score = Set(input.grade_score);
        active.description = Set(input.description);
        active.verification_status = Set("UNVERIFIED".into());
        active.reviewed_by = Set(None);
        active.reviewed_at = Set(None);
        active.rejection_reason = Set(None);
        active.updated_at = Set(now);
        return active.update(db).await.map_err(KabiPayError::from);
    }

    employee_education::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        education_level: Set(input.education_level),
        qualification: Set(input.qualification),
        field_of_study: Set(input.field_of_study),
        institution: Set(input.institution),
        board_university: Set(input.board_university),
        start_date: Set(input.start_date),
        completion_year: Set(input.completion_year),
        grade_score: Set(input.grade_score),
        description: Set(input.description),
        verification_status: Set("UNVERIFIED".into()),
        reviewed_by: Set(None),
        reviewed_at: Set(None),
        rejection_reason: Set(None),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(KabiPayError::from)
}

pub async fn delete_education(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Uuid,
    actor_user_id: Uuid,
) -> KabiPayResult<bool> {
    let row = employee_education::Entity::find_by_id(record_id)
        .filter(employee_education::Column::TenantId.eq(tenant_id))
        .filter(employee_education::Column::EmployeeId.eq(employee_id))
        .filter(employee_education::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeEducation",
            id: record_id.to_string(),
        })?;
    let now = Utc::now();
    let mut active: employee_education::ActiveModel = row.into();
    active.is_deleted = Set(true);
    active.deleted_at = Set(Some(now));
    active.deleted_by = Set(Some(actor_user_id));
    active.updated_at = Set(now);
    active.update(db).await?;
    Ok(true)
}

pub async fn list_work_experience(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Vec<employee_work_experience::Model>> {
    employee_work_experience::Entity::find()
        .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience::Column::EmployeeId.eq(employee_id))
        .filter(employee_work_experience::Column::IsDeleted.eq(false))
        .order_by_desc(employee_work_experience::Column::IsCurrent)
        .order_by_desc(employee_work_experience::Column::StartDate)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn find_work_experience(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_id: Uuid,
) -> KabiPayResult<Option<employee_work_experience::Model>> {
    employee_work_experience::Entity::find_by_id(record_id)
        .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn save_work_experience(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Option<Uuid>,
    input: WorkExperienceRecordInput,
) -> KabiPayResult<employee_work_experience::Model> {
    let input = validate_work(input)?;
    let now = Utc::now();
    let id = record_id.unwrap_or_else(Uuid::new_v4);
    if let Some(record_id) = record_id {
        let existing = employee_work_experience::Entity::find_by_id(record_id)
            .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
            .filter(employee_work_experience::Column::EmployeeId.eq(employee_id))
            .filter(employee_work_experience::Column::IsDeleted.eq(false))
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "employeeWorkExperience",
                id: record_id.to_string(),
            })?;
        let mut active: employee_work_experience::ActiveModel = existing.into();
        active.company = Set(input.company);
        active.role_title = Set(input.role_title);
        active.employment_type = Set(input.employment_type);
        active.location = Set(input.location);
        active.start_date = Set(input.start_date);
        active.end_date = Set(input.end_date);
        active.is_current = Set(input.is_current);
        active.description = Set(input.description);
        active.verification_status = Set("UNVERIFIED".into());
        active.reviewed_by = Set(None);
        active.reviewed_at = Set(None);
        active.rejection_reason = Set(None);
        active.updated_at = Set(now);
        return active.update(db).await.map_err(KabiPayError::from);
    }

    employee_work_experience::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        company: Set(input.company),
        role_title: Set(input.role_title),
        employment_type: Set(input.employment_type),
        location: Set(input.location),
        start_date: Set(input.start_date),
        end_date: Set(input.end_date),
        is_current: Set(input.is_current),
        description: Set(input.description),
        verification_status: Set("UNVERIFIED".into()),
        reviewed_by: Set(None),
        reviewed_at: Set(None),
        rejection_reason: Set(None),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(KabiPayError::from)
}

pub async fn delete_work_experience(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Uuid,
    actor_user_id: Uuid,
) -> KabiPayResult<bool> {
    let row = employee_work_experience::Entity::find_by_id(record_id)
        .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience::Column::EmployeeId.eq(employee_id))
        .filter(employee_work_experience::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeWorkExperience",
            id: record_id.to_string(),
        })?;
    let now = Utc::now();
    let mut active: employee_work_experience::ActiveModel = row.into();
    active.is_deleted = Set(true);
    active.deleted_at = Set(Some(now));
    active.deleted_by = Set(Some(actor_user_id));
    active.updated_at = Set(now);
    active.update(db).await?;
    Ok(true)
}

async fn validate_document_owner(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    document_id: Uuid,
) -> KabiPayResult<()> {
    let found = employee_document::Entity::find_by_id(document_id)
        .filter(employee_document::Column::TenantId.eq(tenant_id))
        .filter(employee_document::Column::EmployeeId.eq(employee_id))
        .filter(employee_document::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .is_some();
    if !found {
        return Err(KabiPayError::Validation(
            "evidence document does not belong to this employee".into(),
        ));
    }
    Ok(())
}

pub async fn link_education_evidence(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Uuid,
    document_id: Uuid,
) -> KabiPayResult<employee_education::Model> {
    validate_document_owner(db, tenant_id, employee_id, document_id).await?;
    let row = employee_education::Entity::find_by_id(record_id)
        .filter(employee_education::Column::TenantId.eq(tenant_id))
        .filter(employee_education::Column::EmployeeId.eq(employee_id))
        .filter(employee_education::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeEducation",
            id: record_id.to_string(),
        })?;
    let existing_education_link = employee_education_document::Entity::find()
        .filter(employee_education_document::Column::TenantId.eq(tenant_id))
        .filter(employee_education_document::Column::EmployeeDocumentId.eq(document_id))
        .one(db)
        .await?;
    if existing_education_link
        .as_ref()
        .is_some_and(|link| link.employee_education_id != record_id)
        || employee_work_experience_document::Entity::find()
            .filter(employee_work_experience_document::Column::TenantId.eq(tenant_id))
            .filter(employee_work_experience_document::Column::EmployeeDocumentId.eq(document_id))
            .one(db)
            .await?
            .is_some()
    {
        return Err(KabiPayError::Conflict(
            "an evidence document can only be linked to one profile record".into(),
        ));
    }
    let exists = existing_education_link.is_some();
    let now = Utc::now();
    if !exists {
        employee_education_document::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_education_id: Set(record_id),
            employee_document_id: Set(document_id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
    }
    let mut active: employee_education::ActiveModel = row.into();
    active.verification_status = Set("PENDING".into());
    active.reviewed_by = Set(None);
    active.reviewed_at = Set(None);
    active.rejection_reason = Set(None);
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)
}

pub async fn link_work_evidence(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    record_id: Uuid,
    document_id: Uuid,
) -> KabiPayResult<employee_work_experience::Model> {
    validate_document_owner(db, tenant_id, employee_id, document_id).await?;
    let row = employee_work_experience::Entity::find_by_id(record_id)
        .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience::Column::EmployeeId.eq(employee_id))
        .filter(employee_work_experience::Column::IsDeleted.eq(false))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeWorkExperience",
            id: record_id.to_string(),
        })?;
    let existing_work_link = employee_work_experience_document::Entity::find()
        .filter(employee_work_experience_document::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience_document::Column::EmployeeDocumentId.eq(document_id))
        .one(db)
        .await?;
    if existing_work_link
        .as_ref()
        .is_some_and(|link| link.employee_work_experience_id != record_id)
        || employee_education_document::Entity::find()
            .filter(employee_education_document::Column::TenantId.eq(tenant_id))
            .filter(employee_education_document::Column::EmployeeDocumentId.eq(document_id))
            .one(db)
            .await?
            .is_some()
    {
        return Err(KabiPayError::Conflict(
            "an evidence document can only be linked to one profile record".into(),
        ));
    }
    let exists = existing_work_link.is_some();
    let now = Utc::now();
    if !exists {
        employee_work_experience_document::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            employee_work_experience_id: Set(record_id),
            employee_document_id: Set(document_id),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
    }
    let mut active: employee_work_experience::ActiveModel = row.into();
    active.verification_status = Set("PENDING".into());
    active.reviewed_by = Set(None);
    active.reviewed_at = Set(None);
    active.rejection_reason = Set(None);
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)
}

pub async fn education_evidence_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    Ok(employee_education_document::Entity::find()
        .filter(employee_education_document::Column::TenantId.eq(tenant_id))
        .filter(employee_education_document::Column::EmployeeEducationId.eq(record_id))
        .order_by_asc(employee_education_document::Column::CreatedAt)
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.employee_document_id)
        .collect())
}

pub async fn education_evidence_ids_by_record(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, Vec<Uuid>>> {
    if record_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = employee_education_document::Entity::find()
        .filter(employee_education_document::Column::TenantId.eq(tenant_id))
        .filter(employee_education_document::Column::EmployeeEducationId.is_in(record_ids.iter().copied()))
        .order_by_asc(employee_education_document::Column::CreatedAt)
        .all(db)
        .await?;
    let mut output = HashMap::new();
    for row in rows {
        output.entry(row.employee_education_id).or_insert_with(Vec::new).push(row.employee_document_id);
    }
    Ok(output)
}

pub async fn work_evidence_ids(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_id: Uuid,
) -> KabiPayResult<Vec<Uuid>> {
    Ok(employee_work_experience_document::Entity::find()
        .filter(employee_work_experience_document::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience_document::Column::EmployeeWorkExperienceId.eq(record_id))
        .order_by_asc(employee_work_experience_document::Column::CreatedAt)
        .all(db)
        .await?
        .into_iter()
        .map(|row| row.employee_document_id)
        .collect())
}

pub async fn work_evidence_ids_by_record(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_ids: &[Uuid],
) -> KabiPayResult<HashMap<Uuid, Vec<Uuid>>> {
    if record_ids.is_empty() {
        return Ok(HashMap::new());
    }
    let rows = employee_work_experience_document::Entity::find()
        .filter(employee_work_experience_document::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience_document::Column::EmployeeWorkExperienceId.is_in(record_ids.iter().copied()))
        .order_by_asc(employee_work_experience_document::Column::CreatedAt)
        .all(db)
        .await?;
    let mut output = HashMap::new();
    for row in rows {
        output.entry(row.employee_work_experience_id).or_insert_with(Vec::new).push(row.employee_document_id);
    }
    Ok(output)
}

#[cfg(test)]
mod validation_tests {
    use super::*;

    #[test]
    fn rejects_future_work_dates() {
        let input = WorkExperienceRecordInput {
            company: "Example".into(),
            role_title: "Engineer".into(),
            employment_type: None,
            location: None,
            start_date: Utc::now().date_naive() + chrono::Days::new(1),
            end_date: None,
            is_current: true,
            description: None,
        };
        assert!(validate_work(input).is_err());
    }

    #[test]
    fn rejects_oversized_optional_fields() {
        let input = EducationRecordInput {
            education_level: "UNDERGRADUATE".into(),
            qualification: "BSc".into(),
            field_of_study: Some("x".repeat(256)),
            institution: "Example".into(),
            board_university: None,
            start_date: None,
            completion_year: Utc::now().year(),
            grade_score: None,
            description: None,
        };
        assert!(validate_education(input).is_err());
    }
}

pub async fn review_education(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_id: Uuid,
    reviewer_user_id: Uuid,
    approved: bool,
    rejection_reason: Option<String>,
) -> KabiPayResult<employee_education::Model> {
    let txn = db.begin().await?;
    let row = employee_education::Entity::find_by_id(record_id)
        .filter(employee_education::Column::TenantId.eq(tenant_id))
        .filter(employee_education::Column::IsDeleted.eq(false))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeEducation",
            id: record_id.to_string(),
        })?;
    if row.verification_status != "PENDING" {
        return Err(KabiPayError::Conflict(
            "only education evidence awaiting review can be resolved".into(),
        ));
    }
    let reason = optional_trimmed(rejection_reason);
    if !approved && reason.is_none() {
        return Err(KabiPayError::Validation(
            "rejectionReason is required when rejecting education evidence".into(),
        ));
    }
    let now = Utc::now();
    let links = employee_education_document::Entity::find()
        .filter(employee_education_document::Column::TenantId.eq(tenant_id))
        .filter(employee_education_document::Column::EmployeeEducationId.eq(record_id))
        .all(&txn)
        .await?;
    if links.is_empty() {
        return Err(KabiPayError::Validation(
            "education evidence must be attached before review".into(),
        ));
    }
    for document_id in links.into_iter().map(|link| link.employee_document_id) {
        if let Some(document) = employee_document::Entity::find_by_id(document_id)
            .filter(employee_document::Column::TenantId.eq(tenant_id))
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
    let employee_id = row.employee_id;
    let mut active: employee_education::ActiveModel = row.into();
    active.verification_status = Set(if approved { "VERIFIED" } else { "REJECTED" }.into());
    active.reviewed_by = Set(Some(reviewer_user_id));
    active.reviewed_at = Set(Some(now));
    active.rejection_reason = Set(if approved { None } else { reason });
    active.updated_at = Set(now);
    active.update(&txn).await?;
    record_review_decision(&txn, tenant_id, employee_id, record_id, reviewer_user_id, "employee_education", approved, now).await?;
    txn.commit().await?;
    employee_education::Entity::find_by_id(record_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("reviewed education record missing".into()))
}

pub async fn review_work_experience(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    record_id: Uuid,
    reviewer_user_id: Uuid,
    approved: bool,
    rejection_reason: Option<String>,
) -> KabiPayResult<employee_work_experience::Model> {
    let txn = db.begin().await?;
    let row = employee_work_experience::Entity::find_by_id(record_id)
        .filter(employee_work_experience::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience::Column::IsDeleted.eq(false))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeWorkExperience",
            id: record_id.to_string(),
        })?;
    if row.verification_status != "PENDING" {
        return Err(KabiPayError::Conflict(
            "only work evidence awaiting review can be resolved".into(),
        ));
    }
    let reason = optional_trimmed(rejection_reason);
    if !approved && reason.is_none() {
        return Err(KabiPayError::Validation(
            "rejectionReason is required when rejecting work evidence".into(),
        ));
    }
    let now = Utc::now();
    let links = employee_work_experience_document::Entity::find()
        .filter(employee_work_experience_document::Column::TenantId.eq(tenant_id))
        .filter(employee_work_experience_document::Column::EmployeeWorkExperienceId.eq(record_id))
        .all(&txn)
        .await?;
    if links.is_empty() {
        return Err(KabiPayError::Validation(
            "work experience evidence must be attached before review".into(),
        ));
    }
    for document_id in links.into_iter().map(|link| link.employee_document_id) {
        if let Some(document) = employee_document::Entity::find_by_id(document_id)
            .filter(employee_document::Column::TenantId.eq(tenant_id))
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
    let employee_id = row.employee_id;
    let mut active: employee_work_experience::ActiveModel = row.into();
    active.verification_status = Set(if approved { "VERIFIED" } else { "REJECTED" }.into());
    active.reviewed_by = Set(Some(reviewer_user_id));
    active.reviewed_at = Set(Some(now));
    active.rejection_reason = Set(if approved { None } else { reason });
    active.updated_at = Set(now);
    active.update(&txn).await?;
    record_review_decision(&txn, tenant_id, employee_id, record_id, reviewer_user_id, "employee_work_experience", approved, now).await?;
    txn.commit().await?;
    employee_work_experience::Entity::find_by_id(record_id)
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::Internal("reviewed work experience record missing".into()))
}
