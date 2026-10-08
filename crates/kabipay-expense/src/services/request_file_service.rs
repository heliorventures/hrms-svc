//! Shared evidence rules for new expense and travel submissions.

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

const MAX_EVIDENCE_BYTES: i64 = 6 * 1024 * 1024;

pub fn required_file_id(file_id: Option<Uuid>) -> KabiPayResult<Uuid> {
    file_id.ok_or_else(|| KabiPayError::Validation("a supporting file is required".into()))
}

pub fn validate_submission_file(
    file: &file_storage::Model,
    tenant_id: Uuid,
    uploader_user_id: Uuid,
) -> KabiPayResult<()> {
    if file.tenant_id != tenant_id || file.uploaded_by != Some(uploader_user_id) {
        return Err(KabiPayError::Validation(
            "supporting file must be uploaded by the submitting user in this company".into(),
        ));
    }
    if file.is_public || !matches!(file.provider.as_str(), "LOCAL" | "S3") {
        return Err(KabiPayError::Validation(
            "supporting file must be a private tenant upload".into(),
        ));
    }
    if !matches!(
        file.mime_type.as_deref(),
        Some("application/pdf" | "image/jpeg" | "image/png")
    ) {
        return Err(KabiPayError::Validation(
            "supporting file must be a PDF, JPG, or PNG".into(),
        ));
    }
    if !file
        .file_size_bytes
        .is_some_and(|size| size > 0 && size <= MAX_EVIDENCE_BYTES)
    {
        return Err(KabiPayError::Validation(
            "supporting file must contain data and be 6 MB or smaller".into(),
        ));
    }
    if file.storage_path.trim().is_empty() {
        return Err(KabiPayError::Validation(
            "supporting file storage reference is missing".into(),
        ));
    }
    Ok(())
}

pub async fn require_submission_file(
    db: &impl ConnectionTrait,
    tenant_id: Uuid,
    uploader_user_id: Uuid,
    file_id: Uuid,
) -> KabiPayResult<()> {
    let file = file_storage::Entity::find_by_id(file_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| {
            KabiPayError::Validation("supporting file was not found in this company".into())
        })?;
    validate_submission_file(&file, tenant_id, uploader_user_id)
}
