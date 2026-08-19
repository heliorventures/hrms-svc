//! `file_storage` + `employee_document` writes: **LOCAL** disk or **S3-compatible** object storage
//! (Cloudflare R2, AWS S3, MinIO, …). See `object_store::config` for environment variables.

use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Duration, Utc};
use kabipay_common::private_file_cleanup::{
    enqueue_and_delete_private_file, enqueue_private_file_cleanup_coordinates,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    QuerySelect, Select, TransactionTrait,
};
use uuid::Uuid;

use super::object_store::{
    FileStorageMode, S3CompatSettings, ensure_tenant_bucket, s3_operator_for_bucket, s3_put,
    s3_read, tenant_bucket_name, PROVIDER_S3_COMPAT,
};
use crate::entities::d0008_document_system::employee_document;
use crate::entities::d0029_file_storage::file_storage;
use crate::entities::d0059_file_upload_stage::file_upload_stage;

const PROVIDER_LOCAL: &str = "LOCAL";
const MAX_BYTES: usize = 6 * 1024 * 1024;
const ALLOWED_DOCUMENT_MIME_TYPES: &[&str] = &["application/pdf", "image/jpeg", "image/png"];
pub const COMPANY_DOCUMENT_UPLOAD_PURPOSE: &str = "COMPANY_DOCUMENT";
const COMPANY_DOCUMENT_UPLOAD_TTL_MINUTES: i64 = 15;

/// Persist an object-only tombstone if the write completed but the metadata transaction failed.
/// If the tenant DB is unavailable too, only an opaque correlation/error class is logged.
async fn enqueue_failed_object_write_cleanup(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    provider: &str,
    bucket: Option<&str>,
    storage_path: &str,
) -> KabiPayResult<()> {
    let correlation_id = Uuid::new_v4();
    if let Err(error) = enqueue_private_file_cleanup_coordinates(
        db,
        tenant_id,
        provider,
        bucket,
        storage_path,
    )
    .await
    {
        tracing::error!(
            tenant_id = %tenant_id,
            cleanup_correlation_id = %correlation_id,
            error_class = error.code(),
            "private file cleanup tombstone could not be recorded"
        );
        return Err(KabiPayError::Internal(
            "private file cleanup could not be scheduled".into(),
        ));
    }
    Ok(())
}

pub struct StagedCompanyDocumentUpload {
    pub stage: file_upload_stage::Model,
    pub file: file_storage::Model,
}

pub fn local_file_root() -> PathBuf {
    let root =
        std::env::var("KABIPAY_LOCAL_FILE_ROOT").unwrap_or_else(|_| "data/tenant_files".into());
    PathBuf::from(root)
}

fn local_fallback_enabled() -> bool {
    // Object storage is preferred, but a transient/unavailable bucket must not make an
    // otherwise valid employee or company upload disappear. Local storage is therefore the
    // default fallback; operators can explicitly disable it when fail-closed storage policy is
    // required.
    if let Ok(value) = std::env::var("KABIPAY_FILE_STORAGE_FALLBACK") {
        return !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "none" | "disabled" | "off"
        );
    }
    if let Ok(value) = std::env::var("KABIPAY_FILE_STORAGE_LOCAL_FALLBACK") {
        return matches!(value.trim().to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "local" | "disk");
    }
    true
}

fn safe_filename(original_filename: &str) -> String {
    let sanitized = original_filename
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '.' | '-' | '_') {
                ch
            } else {
                '-'
            }
        })
        .collect::<String>()
        .trim_matches(|ch| ch == '.' || ch == '-')
        .chars()
        .take(120)
        .collect::<String>();
    if sanitized.is_empty() {
        "document".to_string()
    } else {
        sanitized
    }
}

fn storage_path_for(
    tenant_id: Uuid,
    owner_user_id: Option<Uuid>,
    category: &str,
    file_id: Uuid,
    original_filename: &str,
) -> String {
    let owner = owner_user_id
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unassigned".to_string());
    format!(
        "tenants/{tenant_id}/users/{owner}/{category}/{file_id}/{}",
        safe_filename(original_filename)
    )
}

fn absolute_storage_path(storage_path: &str) -> KabiPayResult<PathBuf> {
    if storage_path.contains('\\')
        || Path::new(storage_path).components().any(|part| {
            matches!(
                part,
                Component::ParentDir | Component::RootDir | Component::Prefix(_)
            )
        })
    {
        return Err(KabiPayError::Validation("invalid file path".into()));
    }
    Ok(local_file_root().join(storage_path))
}

/// Read bytes for `GET /files/employee-document`. Uses row metadata (not only current env) so
/// old local files still work after switching to R2.
pub async fn read_stored_file_bytes(
    file_root: &Path,
    row: &file_storage::Model,
) -> KabiPayResult<Vec<u8>> {
    if row.provider == PROVIDER_LOCAL {
        if row.storage_path.contains('\\')
            || Path::new(&row.storage_path).components().any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(KabiPayError::Validation("invalid file path".into()));
        }
        let full = file_root.join(&row.storage_path);
        return match tokio::fs::read(&full).await {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(KabiPayError::NotFound {
                entity: "document",
                id: "requested".to_string(),
            }),
            Err(e) => Err(KabiPayError::Internal(format!("read local file: {e}"))),
        };
    }
    if row.provider == PROVIDER_S3_COMPAT {
        let cfg = S3CompatSettings::from_env()?;
        let b = row
            .bucket
            .as_ref()
            .ok_or_else(|| KabiPayError::Internal("S3 file missing bucket name in DB".into()))?;
        let op = s3_operator_for_bucket(&cfg, b)?;
        return s3_read(&op, &row.storage_path).await;
    }
    Err(KabiPayError::Validation(format!(
        "unsupported file_storage.provider: {}",
        row.provider
    )))
}

/// Persist `bytes` to disk or object storage, then `file_storage` + `employee_document`.
/// When `hr_auto_approve`, status is **`APPROVED`** and verifier timestamps use the uploader.
pub async fn upload_employee_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    document_type_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
    hr_auto_approve: bool,
) -> KabiPayResult<employee_document::Model> {
    validate_upload_bytes(&bytes)?;
    validate_supported_upload_type(&mime_type, &bytes, "document")?;

    let mode = FileStorageMode::from_env();
    match mode {
        FileStorageMode::Local => {
            upload_local(
                db,
                tenant_id,
                employee_id,
                document_type_id,
                uploader_user_id,
                original_filename,
                mime_type,
                bytes,
                hr_auto_approve,
            )
            .await
        }
        FileStorageMode::S3Compat => {
            let s3_result = upload_s3_employee_document(
                db,
                tenant_id,
                employee_id,
                document_type_id,
                uploader_user_id,
                original_filename.clone(),
                mime_type.clone(),
                bytes.clone(),
                hr_auto_approve,
            )
            .await;
            match s3_result {
                Ok(row) => Ok(row),
                Err(error) if local_fallback_enabled() => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        code = error.code(),
                        "S3 employee document upload failed; using configured local fallback"
                    );
                    upload_local(
                        db,
                        tenant_id,
                        employee_id,
                        document_type_id,
                        uploader_user_id,
                        original_filename,
                        mime_type,
                        bytes,
                        hr_auto_approve,
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        }
        FileStorageMode::AzureBlob => Err(KabiPayError::Validation(
            "KABIPAY_FILE_STORAGE_MODE=azure is not implemented yet. Use local, or s3_compat for R2/S3/MinIO."
                .into(),
        )),
    }
}

/// Compensate a newly uploaded document when its owning business record could not be linked.
/// This is only for same-request failures; callers must never use it as a general delete API.
pub async fn cleanup_unlinked_employee_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    document_id: Uuid,
) -> KabiPayResult<()> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let document = employee_document::Entity::find_by_id(document_id)
        .filter(employee_document::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeDocument",
            id: document_id.to_string(),
        })?;
    let stored_file = match document.file_storage_id {
        Some(file_id) => file_storage::Entity::find_by_id(file_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?,
        None => None,
    };

    employee_document::Entity::delete_by_id(document.id)
        .exec(&txn)
        .await
        .map_err(KabiPayError::from)?;
    if let Some(stored_file) = stored_file {
        enqueue_and_delete_private_file(&txn, tenant_id, &stored_file).await?;
    }
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(())
}

fn unclaimed_company_stage_query(
    stage_id: Uuid,
    tenant_id: Uuid,
    creator_user_id: Uuid,
    expires_at: DateTime<Utc>,
) -> Select<file_upload_stage::Entity> {
    file_upload_stage::Entity::find()
        .filter(file_upload_stage::Column::Id.eq(stage_id))
        .filter(file_upload_stage::Column::TenantId.eq(tenant_id))
        .filter(file_upload_stage::Column::Purpose.eq(COMPANY_DOCUMENT_UPLOAD_PURPOSE))
        .filter(file_upload_stage::Column::CreatedBy.eq(creator_user_id))
        .filter(file_upload_stage::Column::ExpiresAt.eq(expires_at))
        .filter(file_upload_stage::Column::ClaimedAt.is_null())
        .filter(file_upload_stage::Column::CleanupBlockedAt.is_null())
}

/// Store a company-document upload behind an opaque, creator-bound stage ID.
/// The underlying `file_storage.id` is never returned to the caller.
pub async fn upload_company_document_file(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    creator_user_id: Uuid,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<StagedCompanyDocumentUpload> {
    let file = upload_tenant_file(
        db,
        tenant_id,
        Some(creator_user_id),
        original_filename,
        mime_type,
        bytes,
    )
    .await?;
    let now = Utc::now();
    let stage = file_upload_stage::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        file_storage_id: Set(file.id),
        purpose: Set(COMPANY_DOCUMENT_UPLOAD_PURPOSE.to_string()),
        created_by: Set(creator_user_id),
        expires_at: Set(now + Duration::minutes(COMPANY_DOCUMENT_UPLOAD_TTL_MINUTES)),
        claimed_at: Set(None),
        claimed_resource_id: Set(None),
        cleanup_blocked_at: Set(None),
        cleanup_error_class: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await;

    match stage {
        Ok(stage) => Ok(StagedCompanyDocumentUpload { stage, file }),
        Err(error) => {
            cleanup_known_new_file(db, tenant_id, &file).await;
            Err(KabiPayError::from(error))
        }
    }
}

/// Compensate only an operation-owned company upload that is still unclaimed.
/// The exact expiry from the locked stage is repeated in the delete predicate so
/// a replaced/recreated lease cannot be removed by a stale cleanup attempt.
pub async fn cleanup_unlinked_company_upload(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    stage_id: Uuid,
    uploader_user_id: Uuid,
) -> KabiPayResult<bool> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let Some(stage_lease) = file_upload_stage::Entity::find()
        .filter(file_upload_stage::Column::Id.eq(stage_id))
        .filter(file_upload_stage::Column::TenantId.eq(tenant_id))
        .filter(file_upload_stage::Column::Purpose.eq(COMPANY_DOCUMENT_UPLOAD_PURPOSE))
        .filter(file_upload_stage::Column::CreatedBy.eq(uploader_user_id))
        .filter(file_upload_stage::Column::ClaimedAt.is_null())
        .filter(file_upload_stage::Column::CleanupBlockedAt.is_null())
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
    else {
        return Ok(false);
    };
    let Some(stage) = unclaimed_company_stage_query(
        stage_id,
        tenant_id,
        uploader_user_id,
        stage_lease.expires_at.clone(),
    )
    .lock_exclusive()
    .one(&txn)
    .await
    .map_err(KabiPayError::from)?
    else {
        return Ok(false);
    };

    let Some(stored_file) = file_storage::Entity::find_by_id(stage.file_storage_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
    else {
        return Err(KabiPayError::Internal(
            "staged upload file metadata is missing".into(),
        ));
    };

    let deleted_stage = file_upload_stage::Entity::delete_many()
        .filter(file_upload_stage::Column::Id.eq(stage.id))
        .filter(file_upload_stage::Column::TenantId.eq(tenant_id))
        .filter(file_upload_stage::Column::Purpose.eq(COMPANY_DOCUMENT_UPLOAD_PURPOSE))
        .filter(file_upload_stage::Column::CreatedBy.eq(uploader_user_id))
        .filter(file_upload_stage::Column::ExpiresAt.eq(stage.expires_at.clone()))
        .filter(file_upload_stage::Column::ClaimedAt.is_null())
        .filter(file_upload_stage::Column::CleanupBlockedAt.is_null())
        .exec(&txn)
        .await
        .map_err(KabiPayError::from)?;
    if deleted_stage.rows_affected == 0 {
        return Ok(false);
    }
    enqueue_and_delete_private_file(&txn, tenant_id, &stored_file).await?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(true)
}

async fn cleanup_known_new_file(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    stored_file: &file_storage::Model,
) {
    let result = async {
        let txn = db.begin().await.map_err(KabiPayError::from)?;
        enqueue_and_delete_private_file(&txn, tenant_id, stored_file).await?;
        txn.commit().await.map_err(KabiPayError::from)
    }
    .await;
    if let Err(error) = result {
        tracing::warn!(
            tenant_id = %tenant_id,
            code = error.code(),
            "new company upload cleanup could not be queued"
        );
    }
}

/// Persist a tenant-scoped file without attaching it to an employee document. This is used by
/// HRMS modules that store a `file_storage_id` directly, such as expense receipts and tax proofs.
pub async fn upload_tenant_file(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<file_storage::Model> {
    validate_upload_bytes(&bytes)?;
    validate_supported_upload_type(&mime_type, &bytes, "tenant file")?;

    let mode = FileStorageMode::from_env();
    match mode {
        FileStorageMode::Local => {
            let file_id = Uuid::new_v4();
            let now = Utc::now();
            let rel = storage_path_for(
                tenant_id,
                uploader_user_id,
                "documents",
                file_id,
                &original_filename,
            );
            let path = absolute_storage_path(&rel)?;
            if let Some(parent) = path.parent() {
                tokio::fs::create_dir_all(parent).await.map_err(|e: std::io::Error| {
                    KabiPayError::Internal(format!("create_dir_all: {e}"))
                })?;
            }
            tokio::fs::write(&path, &bytes)
                .await
                .map_err(|e| KabiPayError::Internal(format!("write local file: {e}")))?;
            let inserted = insert_file_storage(
                db,
                tenant_id,
                uploader_user_id,
                original_filename,
                mime_type,
                file_id,
                now,
                None,
                rel.clone(),
                bytes.len() as i64,
            )
            .await;
            match inserted {
                Ok(file) => Ok(file),
                Err(error) => {
                    enqueue_failed_object_write_cleanup(
                        db, tenant_id, PROVIDER_LOCAL, None, &rel,
                    )
                    .await?;
                    Err(error)
                }
            }
        }
        FileStorageMode::S3Compat => {
            let s3_result = upload_s3_tenant_file(
                db,
                tenant_id,
                uploader_user_id,
                original_filename.clone(),
                mime_type.clone(),
                bytes.clone(),
            )
            .await;
            match s3_result {
                Ok(row) => Ok(row),
                Err(error) if local_fallback_enabled() => {
                    tracing::warn!(
                        tenant_id = %tenant_id,
                        code = error.code(),
                        "S3 tenant file upload failed; using configured local fallback"
                    );
                    upload_local_tenant_file(
                        db,
                        tenant_id,
                        uploader_user_id,
                        original_filename,
                        mime_type,
                        bytes,
                    )
                    .await
                }
                Err(error) => Err(error),
            }
        }
        FileStorageMode::AzureBlob => Err(KabiPayError::Validation(
            "KABIPAY_FILE_STORAGE_MODE=azure is not implemented yet. Use local, or s3_compat for R2/S3/MinIO."
                .into(),
        )),
    }
}

fn validate_upload_bytes(bytes: &[u8]) -> KabiPayResult<()> {
    if bytes.is_empty() {
        return Err(KabiPayError::Validation(
            "upload file content must not be empty".into(),
        ));
    }
    if bytes.len() > MAX_BYTES {
        return Err(KabiPayError::Validation(format!(
            "file exceeds max size of {} bytes",
            MAX_BYTES
        )));
    }
    Ok(())
}

fn validate_supported_upload_type(
    mime_type: &Option<String>,
    bytes: &[u8],
    label: &str,
) -> KabiPayResult<()> {
    let mime = mime_type
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !ALLOWED_DOCUMENT_MIME_TYPES.contains(&mime.as_str()) {
        return Err(KabiPayError::Validation(format!(
            "{label} must be a PDF, JPG, or PNG file"
        )));
    }
    let matches_magic = match mime.as_str() {
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        _ => false,
    };
    if !matches_magic {
        return Err(KabiPayError::Validation(format!(
            "{label} content does not match its declared file type"
        )));
    }
    Ok(())
}

async fn upload_local(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    document_type_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
    hr_auto_approve: bool,
) -> KabiPayResult<employee_document::Model> {
    let file_id = Uuid::new_v4();
    let doc_id = Uuid::new_v4();
    let now = Utc::now();
    let rel = storage_path_for(
        tenant_id,
        Some(employee_id),
        "documents",
        file_id,
        &original_filename,
    );
    let path = absolute_storage_path(&rel)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e: std::io::Error| KabiPayError::Internal(format!("create_dir_all: {e}")))?;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| KabiPayError::Internal(format!("write local file: {e}")))?;
    let sz = bytes.len() as i64;

    let inserted = insert_fs_doc(
        db,
        tenant_id,
        employee_id,
        document_type_id,
        uploader_user_id,
        original_filename,
        mime_type,
        file_id,
        doc_id,
        now,
        None,
        rel.clone(),
        sz,
        hr_auto_approve,
    )
    .await;
    match inserted {
        Ok(document) => Ok(document),
        Err(error) => {
            enqueue_failed_object_write_cleanup(db, tenant_id, PROVIDER_LOCAL, None, &rel).await?;
            Err(error)
        }
    }
}

async fn upload_s3_employee_document(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    document_type_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
    hr_auto_approve: bool,
) -> KabiPayResult<employee_document::Model> {
    let cfg = S3CompatSettings::from_env()?;
    let file_id = Uuid::new_v4();
    let doc_id = Uuid::new_v4();
    let now = Utc::now();
    let bucket = if cfg.per_tenant_bucket {
        tenant_bucket_name(tenant_id, &cfg.bucket_prefix)
    } else {
        cfg.default_bucket
            .as_ref()
            .expect("validated in S3CompatSettings::from_env")
            .clone()
    };
    ensure_tenant_bucket(&cfg, &bucket).await?;
    let storage_path = storage_path_for(
        tenant_id,
        Some(employee_id),
        "documents",
        file_id,
        &original_filename,
    );
    let sz = bytes.len() as i64;
    let op = s3_operator_for_bucket(&cfg, &bucket)?;
    s3_put(
        &op,
        &storage_path,
        bytes,
        mime_type.as_deref().filter(|s| !s.is_empty()),
    )
    .await?;
    let inserted = insert_fs_doc(
        db,
        tenant_id,
        employee_id,
        document_type_id,
        uploader_user_id,
        original_filename,
        mime_type,
        file_id,
        doc_id,
        now,
        Some(bucket.clone()),
        storage_path.clone(),
        sz,
        hr_auto_approve,
    )
    .await;
    match inserted {
        Ok(document) => Ok(document),
        Err(error) => {
            enqueue_failed_object_write_cleanup(
                db,
                tenant_id,
                PROVIDER_S3_COMPAT,
                Some(&bucket),
                &storage_path,
            )
            .await?;
            Err(error)
        }
    }
}

async fn upload_local_tenant_file(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<file_storage::Model> {
    let file_id = Uuid::new_v4();
    let now = Utc::now();
    let rel = storage_path_for(
        tenant_id,
        uploader_user_id,
        "documents",
        file_id,
        &original_filename,
    );
    let path = absolute_storage_path(&rel)?;
    if let Some(parent) = path.parent() {
        tokio::fs::create_dir_all(parent)
            .await
            .map_err(|e: std::io::Error| KabiPayError::Internal(format!("create_dir_all: {e}")))?;
    }
    tokio::fs::write(&path, &bytes)
        .await
        .map_err(|e| KabiPayError::Internal(format!("write local file: {e}")))?;
    let inserted = insert_file_storage(
        db,
        tenant_id,
        uploader_user_id,
        original_filename,
        mime_type,
        file_id,
        now,
        None,
        rel.clone(),
        bytes.len() as i64,
    )
    .await;
    match inserted {
        Ok(file) => Ok(file),
        Err(error) => {
            enqueue_failed_object_write_cleanup(db, tenant_id, PROVIDER_LOCAL, None, &rel).await?;
            Err(error)
        }
    }
}

async fn upload_s3_tenant_file(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<file_storage::Model> {
    let cfg = S3CompatSettings::from_env()?;
    let file_id = Uuid::new_v4();
    let now = Utc::now();
    let bucket = if cfg.per_tenant_bucket {
        tenant_bucket_name(tenant_id, &cfg.bucket_prefix)
    } else {
        cfg.default_bucket
            .as_ref()
            .expect("validated in S3CompatSettings::from_env")
            .clone()
    };
    ensure_tenant_bucket(&cfg, &bucket).await?;
    let storage_path = storage_path_for(
        tenant_id,
        uploader_user_id,
        "documents",
        file_id,
        &original_filename,
    );
    let size = bytes.len() as i64;
    let op = s3_operator_for_bucket(&cfg, &bucket)?;
    s3_put(
        &op,
        &storage_path,
        bytes,
        mime_type.as_deref().filter(|s| !s.is_empty()),
    )
    .await?;
    let inserted = insert_file_storage(
        db,
        tenant_id,
        uploader_user_id,
        original_filename,
        mime_type,
        file_id,
        now,
        Some(bucket.clone()),
        storage_path.clone(),
        size,
    )
    .await;
    match inserted {
        Ok(file) => Ok(file),
        Err(error) => {
            enqueue_failed_object_write_cleanup(
                db,
                tenant_id,
                PROVIDER_S3_COMPAT,
                Some(&bucket),
                &storage_path,
            )
            .await?;
            Err(error)
        }
    }
}

async fn insert_fs_doc(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    document_type_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    file_id: Uuid,
    doc_id: Uuid,
    now: chrono::DateTime<Utc>,
    bucket: Option<String>,
    storage_path: String,
    size: i64,
    hr_auto_approve: bool,
) -> KabiPayResult<employee_document::Model> {
    let provider = if bucket.is_some() {
        PROVIDER_S3_COMPAT.into()
    } else {
        PROVIDER_LOCAL.into()
    };
    let txn = db.begin().await?;

    let fs_am = file_storage::ActiveModel {
        id: Set(file_id),
        tenant_id: Set(tenant_id),
        provider: Set(provider),
        bucket: Set(bucket),
        storage_path: Set(storage_path),
        original_filename: Set(Some(original_filename)),
        mime_type: Set(mime_type),
        file_size_bytes: Set(Some(size)),
        is_public: Set(false),
        uploaded_by: Set(uploader_user_id),
        created_at: Set(now),
        updated_at: Set(now),
    };
    fs_am.insert(&txn).await.map_err(KabiPayError::from)?;

    let (status, verified_by, verified_at): (String, Option<Uuid>, Option<chrono::DateTime<Utc>>) =
        if hr_auto_approve {
            (
                "APPROVED".into(),
                uploader_user_id,
                Some(now),
            )
        } else {
            ("PENDING".into(), None, None)
        };

    let am = employee_document::ActiveModel {
        id: Set(doc_id),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        document_type_id: Set(document_type_id),
        file_storage_id: Set(Some(file_id)),
        status: Set(status),
        expiry_date: Set(None),
        workflow_instance_id: Set(None),
        uploaded_at: Set(now),
        verified_by: Set(verified_by),
        verified_at: Set(verified_at),
        is_deleted: Set(false),
        deleted_at: Set(None),
        deleted_by: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(&txn).await.map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;

    employee_document::Entity::find_by_id(doc_id)
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::Internal("inserted employee_document missing".into()))
}

async fn insert_file_storage(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploader_user_id: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    file_id: Uuid,
    now: chrono::DateTime<Utc>,
    bucket: Option<String>,
    storage_path: String,
    size: i64,
) -> KabiPayResult<file_storage::Model> {
    let provider = if bucket.is_some() {
        PROVIDER_S3_COMPAT.into()
    } else {
        PROVIDER_LOCAL.into()
    };
    let fs_am = file_storage::ActiveModel {
        id: Set(file_id),
        tenant_id: Set(tenant_id),
        provider: Set(provider),
        bucket: Set(bucket),
        storage_path: Set(storage_path),
        original_filename: Set(Some(original_filename)),
        mime_type: Set(mime_type),
        file_size_bytes: Set(Some(size)),
        is_public: Set(false),
        uploaded_by: Set(uploader_user_id),
        created_at: Set(now),
        updated_at: Set(now),
    };
    fs_am.insert(db).await.map_err(KabiPayError::from)
}

#[cfg(test)]
mod stage_tests {
    use super::*;
    use chrono::Duration;
    use sea_orm::{DbBackend, QueryTrait};

    #[test]
    fn company_stage_cleanup_query_is_operation_scoped() {
        let stage_id = Uuid::new_v4();
        let tenant_id = Uuid::new_v4();
        let creator = Uuid::new_v4();
        let expires_at = Utc::now() + Duration::minutes(15);
        let statement = unclaimed_company_stage_query(stage_id, tenant_id, creator, expires_at)
            .build(DbBackend::Postgres)
            .to_string();

        assert!(statement.contains(&stage_id.to_string()));
        assert!(statement.contains(&tenant_id.to_string()));
        assert!(statement.contains(&creator.to_string()));
        assert!(statement.contains(COMPANY_DOCUMENT_UPLOAD_PURPOSE));
        assert!(statement.contains("\"claimed_at\" IS NULL"));
        assert!(statement.contains("\"cleanup_blocked_at\" IS NULL"));
        assert!(statement.contains("\"expires_at\""));
    }
}
