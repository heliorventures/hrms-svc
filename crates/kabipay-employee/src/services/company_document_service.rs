//! Company policy, onboarding, and exit-formality documents.

use chrono::{DateTime, Utc};
use kabipay_common::private_file_cleanup::enqueue_and_delete_private_file;
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use crate::entities::d0029_file_storage::file_storage;
use crate::entities::d0056_company_documents::company_document;
use crate::entities::d0059_file_upload_stage::file_upload_stage;
use crate::services::document_file_service::COMPANY_DOCUMENT_UPLOAD_PURPOSE;

const ACTIVE_STATUS: &str = "ACTIVE";
const ARCHIVED_STATUS: &str = "ARCHIVED";
const COMPANY_POLICY_CATEGORY: &str = "COMPANY_POLICY";
const ONBOARDING_CATEGORY: &str = "ONBOARDING";
const EXIT_FORMALITY_CATEGORY: &str = "EXIT_FORMALITY";

pub struct NewCompanyDocument {
    pub category: String,
    pub title: String,
    pub description: Option<String>,
    pub staged_upload_id: Uuid,
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
) -> KabiPayResult<(company_document::Model, file_storage::Model)> {
    let category = normalize_category(&input.category)?;
    let title = normalize_title(&input.title)?;
    let description = normalize_description(input.description);
    let now = Utc::now();
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let stage = file_upload_stage::Entity::find_by_id(input.staged_upload_id)
        .filter(file_upload_stage::Column::TenantId.eq(tenant_id))
        .filter(file_upload_stage::Column::CleanupBlockedAt.is_null())
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(invalid_company_document_stage)?;
    validate_company_document_stage(
        &stage.purpose,
        stage.created_by,
        stage.expires_at.clone(),
        stage.claimed_at.clone(),
        input.uploaded_by,
        now.clone(),
    )?;
    let file = file_storage::Entity::find_by_id(stage.file_storage_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .lock_shared()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "fileStorage",
            id: stage.file_storage_id.to_string(),
        })?;
    if file.uploaded_by != Some(input.uploaded_by) {
        return Err(invalid_company_document_stage());
    }

    let id = Uuid::new_v4();
    let model = company_document::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant_id),
        category: Set(category),
        title: Set(title),
        description: Set(description),
        file_storage_id: Set(file.id),
        status: Set(ACTIVE_STATUS.to_string()),
        visible_to_employees: Set(input.visible_to_employees),
        uploaded_by: Set(Some(input.uploaded_by)),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now.clone()),
        updated_at: Set(now.clone()),
    };
    let row = model.insert(&txn).await.map_err(KabiPayError::from)?;
    let mut active_stage: file_upload_stage::ActiveModel = stage.into();
    active_stage.claimed_at = Set(Some(now.clone()));
    active_stage.claimed_resource_id = Set(Some(row.id));
    active_stage.updated_at = Set(now);
    active_stage
        .update(&txn)
        .await
        .map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok((row, file))
}

fn invalid_company_document_stage() -> KabiPayError {
    KabiPayError::ConflictRule {
        code: "COMPANY_DOCUMENT_UPLOAD_INVALID",
        message: "staged upload is not valid for a company document".into(),
    }
}

fn validate_company_document_stage(
    purpose: &str,
    created_by: Uuid,
    expires_at: DateTime<Utc>,
    claimed_at: Option<DateTime<Utc>>,
    expected_creator: Uuid,
    now: DateTime<Utc>,
) -> KabiPayResult<()> {
    if purpose != COMPANY_DOCUMENT_UPLOAD_PURPOSE || created_by != expected_creator {
        return Err(invalid_company_document_stage());
    }
    if claimed_at.is_some() {
        return Err(KabiPayError::ConflictRule {
            code: "COMPANY_DOCUMENT_UPLOAD_CLAIMED",
            message: "staged upload has already been used".into(),
        });
    }
    if expires_at <= now {
        return Err(KabiPayError::ConflictRule {
            code: "COMPANY_DOCUMENT_UPLOAD_EXPIRED",
            message: "staged upload has expired".into(),
        });
    }
    Ok(())
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
    _deleted_by: Uuid,
) -> KabiPayResult<bool> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let row = company_document::Entity::find_by_id(document_id)
        .filter(company_document::Column::TenantId.eq(tenant_id))
        .filter(company_document::Column::IsDeleted.eq(false))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "companyDocument",
            id: document_id.to_string(),
        })?;
    let file = file_storage::Entity::find_by_id(row.file_storage_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("company document file metadata is missing".into()))?;

    // Claimed stages are audit evidence only while the business record exists. A permanent
    // delete removes that evidence before the RESTRICT FK, then durably tombstones the file.
    file_upload_stage::Entity::delete_many()
        .filter(file_upload_stage::Column::TenantId.eq(tenant_id))
        .filter(file_upload_stage::Column::ClaimedResourceId.eq(document_id))
        .exec(&txn)
        .await
        .map_err(KabiPayError::from)?;
    let deleted = company_document::Entity::delete_by_id(document_id)
        .exec(&txn)
        .await
        .map_err(KabiPayError::from)?;
    if deleted.rows_affected != 1 {
        return Err(KabiPayError::Internal(
            "company document delete affected an unexpected number of rows".into(),
        ));
    }
    enqueue_and_delete_private_file(&txn, tenant_id, &file).await?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(true)
}

/// Find a company document through the caller-selected visibility predicate.
/// `include_hidden` is authorized by the resolver and never inferred from roles here.
pub async fn find_visible_company_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
    include_hidden: bool,
) -> KabiPayResult<Option<company_document::Model>> {
    let mut query = company_document::Entity::find_by_id(document_id)
        .filter(company_document::Column::TenantId.eq(tenant_id))
        .filter(company_document::Column::IsDeleted.eq(false));
    if !include_hidden {
        query = query
            .filter(company_document::Column::VisibleToEmployees.eq(true))
            .filter(company_document::Column::Status.eq(ACTIVE_STATUS));
    }
    query.one(db).await.map_err(KabiPayError::from)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Duration;

    #[test]
    fn company_document_stage_requires_expected_creator_purpose_and_live_lease() {
        let creator = Uuid::new_v4();
        let now = Utc::now();

        assert_eq!(
            validate_company_document_stage(
                "OTHER_PURPOSE",
                creator,
                now + Duration::minutes(1),
                None,
                creator,
                now,
            )
            .unwrap_err()
            .code(),
            "COMPANY_DOCUMENT_UPLOAD_INVALID"
        );
        assert_eq!(
            validate_company_document_stage(
                COMPANY_DOCUMENT_UPLOAD_PURPOSE,
                creator,
                now - Duration::seconds(1),
                None,
                creator,
                now,
            )
            .unwrap_err()
            .code(),
            "COMPANY_DOCUMENT_UPLOAD_EXPIRED"
        );
        assert_eq!(
            validate_company_document_stage(
                COMPANY_DOCUMENT_UPLOAD_PURPOSE,
                creator,
                now + Duration::minutes(1),
                Some(now),
                creator,
                now,
            )
            .unwrap_err()
            .code(),
            "COMPANY_DOCUMENT_UPLOAD_CLAIMED"
        );
        assert!(validate_company_document_stage(
            COMPANY_DOCUMENT_UPLOAD_PURPOSE,
            creator,
            now + Duration::minutes(1),
            None,
            creator,
            now,
        )
        .is_ok());
    }
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
