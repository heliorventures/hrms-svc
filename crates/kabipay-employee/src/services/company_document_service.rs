//! Company policy, onboarding, and exit-formality documents.

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect,
};
use uuid::Uuid;

use crate::entities::d0029_file_storage::file_storage;
use crate::entities::d0056_company_documents::company_document;

const ACTIVE_STATUS: &str = "ACTIVE";
const ARCHIVED_STATUS: &str = "ARCHIVED";
const COMPANY_POLICY_CATEGORY: &str = "COMPANY_POLICY";
const ONBOARDING_CATEGORY: &str = "ONBOARDING";
const EXIT_FORMALITY_CATEGORY: &str = "EXIT_FORMALITY";

pub struct NewCompanyDocument {
    pub category: String,
    pub title: String,
    pub description: Option<String>,
    pub file_storage_id: Uuid,
    pub visible_to_employees: bool,
    pub uploaded_by: Uuid,
}

fn normalize_category(category: &str) -> KabiPayResult<String> {
    let normalized = category.trim().to_ascii_uppercase().replace([' ', '-'], "_");
    match normalized.as_str() {
        COMPANY_POLICY_CATEGORY | ONBOARDING_CATEGORY | EXIT_FORMALITY_CATEGORY => Ok(normalized),
        _ => Err(KabiPayError::Validation(
            "category must be COMPANY_POLICY, ONBOARDING, or EXIT_FORMALITY".into(),
        )),
    }
}

fn normalize_status(status: &str) -> KabiPayResult<String> {
    let normalized = status.trim().to_ascii_uppercase().replace([' ', '-'], "_");
    match normalized.as_str() {
        ACTIVE_STATUS | ARCHIVED_STATUS => Ok(normalized),
        _ => Err(KabiPayError::Validation(
            "status must be ACTIVE or ARCHIVED".into(),
        )),
    }
}

fn normalize_title(title: &str) -> KabiPayResult<String> {
    let trimmed = title.trim();
    if trimmed.is_empty() {
        return Err(KabiPayError::Validation("title is required".into()));
    }
    if trimmed.chars().count() > 255 {
        return Err(KabiPayError::Validation(
            "title must be 255 characters or fewer".into(),
        ));
    }
    Ok(trimmed.to_string())
}

fn normalize_description(description: Option<String>) -> Option<String> {
    description
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
}

pub async fn list_company_documents(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    category: Option<String>,
    active_only: bool,
    include_hidden: bool,
    limit: u64,
) -> KabiPayResult<Vec<company_document::Model>> {
    let mut query = company_document::Entity::find()
        .filter(company_document::Column::TenantId.eq(tenant_id))
        .filter(company_document::Column::IsDeleted.eq(false));

    if let Some(category) = category {
        query = query.filter(company_document::Column::Category.eq(normalize_category(&category)?));
    }
    if active_only {
        query = query.filter(company_document::Column::Status.eq(ACTIVE_STATUS));
    }
    if !include_hidden {
        query = query
            .filter(company_document::Column::VisibleToEmployees.eq(true))
            .filter(company_document::Column::Status.eq(ACTIVE_STATUS));
    }

    query
        .order_by_desc(company_document::Column::UpdatedAt)
        .limit(limit.clamp(1, 200))
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn create_company_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    input: NewCompanyDocument,
) -> KabiPayResult<company_document::Model> {
    let file = file_storage::Entity::find_by_id(input.file_storage_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "fileStorage",
            id: input.file_storage_id.to_string(),
        })?;
    if file.uploaded_by != Some(input.uploaded_by) {
        return Err(KabiPayError::Forbidden(
            "company document must use a file uploaded by the current admin".into(),
        ));
    }

    let now = Utc::now();
    let id = Uuid::new_v4();
    let model = company_document::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        category: Set(normalize_category(&input.category)?),
        title: Set(normalize_title(&input.title)?),
        description: Set(normalize_description(input.description)),
        file_storage_id: Set(input.file_storage_id),
        status: Set(ACTIVE_STATUS.to_string()),
        visible_to_employees: Set(input.visible_to_employees),
        uploaded_by: Set(Some(input.uploaded_by)),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    model.insert(db).await.map_err(KabiPayError::from)
}

pub async fn archive_company_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
) -> KabiPayResult<company_document::Model> {
    let row = company_document::Entity::find_by_id(document_id)
        .filter(company_document::Column::TenantId.eq(tenant_id))
        .filter(company_document::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "companyDocument",
            id: document_id.to_string(),
        })?;
    let now = Utc::now();
    let mut active: company_document::ActiveModel = row.into();
    active.status = Set(normalize_status(ARCHIVED_STATUS)?);
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)
}

pub async fn delete_company_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
    deleted_by: Uuid,
) -> KabiPayResult<bool> {
    let row = company_document::Entity::find_by_id(document_id)
        .filter(company_document::Column::TenantId.eq(tenant_id))
        .filter(company_document::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "companyDocument",
            id: document_id.to_string(),
        })?;
    let now = Utc::now();
    let mut active: company_document::ActiveModel = row.into();
    active.is_deleted = Set(true);
    active.status = Set(ARCHIVED_STATUS.to_string());
    active.deleted_at = Set(Some(now));
    active.deleted_by = Set(Some(deleted_by));
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)?;
    Ok(true)
}

pub async fn find_company_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
) -> KabiPayResult<company_document::Model> {
    company_document::Entity::find_by_id(document_id)
        .filter(company_document::Column::TenantId.eq(tenant_id))
        .filter(company_document::Column::IsDeleted.eq(false))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "companyDocument",
            id: document_id.to_string(),
        })
}

pub async fn map_file_storage_rows(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    rows: &[company_document::Model],
) -> KabiPayResult<std::collections::HashMap<Uuid, file_storage::Model>> {
    let mut ids: Vec<Uuid> = rows.iter().map(|row| row.file_storage_id).collect();
    ids.sort_unstable();
    ids.dedup();
    if ids.is_empty() {
        return Ok(std::collections::HashMap::new());
    }
    let files = file_storage::Entity::find()
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .filter(file_storage::Column::Id.is_in(ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)?;
    Ok(files.into_iter().map(|file| (file.id, file)).collect())
}
