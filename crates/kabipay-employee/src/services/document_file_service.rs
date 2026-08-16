//! `file_storage` + `employee_document` writes: **LOCAL** disk or **S3-compatible** object storage
//! (Cloudflare R2, AWS S3, MinIO, …). See `object_store::config` for environment variables.

use std::path::{Path, PathBuf};

use chrono::Utc;
use kabipay_common::{
    file_download_token::{file_download_claims, public_employee_file_download_url},
    KabiPayError, KabiPayResult,
};
use sea_orm::{
    ActiveModelTrait, ActiveValue::Set, ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter,
    TransactionTrait,
};
use uuid::Uuid;

use super::object_store::{
    FileStorageMode, S3CompatSettings, ensure_tenant_bucket, s3_delete, s3_operator_for_bucket,
    s3_put, s3_read, tenant_bucket_name, PROVIDER_S3_COMPAT,
};
use crate::entities::d0008_document_system::employee_document;
use crate::entities::d0029_file_storage::file_storage;

const PROVIDER_LOCAL: &str = "LOCAL";
const MAX_BYTES: usize = 6 * 1024 * 1024;
const ALLOWED_DOCUMENT_MIME_TYPES: &[&str] = &["application/pdf", "image/jpeg", "image/png"];

pub fn local_file_root() -> PathBuf {
    let root =
        std::env::var("KABIPAY_LOCAL_FILE_ROOT").unwrap_or_else(|_| "data/tenant_files".into());
    PathBuf::from(root)
}

fn absolute_storage_path(tenant_id: Uuid, file_id: Uuid) -> PathBuf {
    let mut p = local_file_root();
    p.push(tenant_id.to_string());
    p.push(format!("{file_id}"));
    p
}

/// Read bytes for `GET /files/employee-document`. Uses row metadata (not only current env) so
/// old local files still work after switching to R2.
pub async fn read_stored_file_bytes(
    file_root: &Path,
    row: &file_storage::Model,
) -> KabiPayResult<Vec<u8>> {
    if row.provider == PROVIDER_LOCAL {
        let full = file_root.join(&row.storage_path);
        if !full.starts_with(file_root) {
            return Err(KabiPayError::Validation("path invalid".into()));
        }
        return match tokio::fs::read(&full).await {
            Ok(b) => Ok(b),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => Err(KabiPayError::NotFound {
                entity: "file_storage",
                id: row.id.to_string(),
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
            let cfg = S3CompatSettings::from_env()?;
            let file_id = Uuid::new_v4();
            let doc_id = Uuid::new_v4();
            let now = Utc::now();
            let (bucket, storage_path): (String, String) = if cfg.per_tenant_bucket {
                let b = tenant_bucket_name(tenant_id, &cfg.bucket_prefix);
                ensure_tenant_bucket(&cfg, &b).await?;
                (b, file_id.to_string())
            } else {
                let b = cfg
                    .default_bucket
                    .as_ref()
                    .expect("validated in S3CompatSettings::from_env")
                    .clone();
                ensure_tenant_bucket(&cfg, &b).await?;
                (b, format!("{}/{}", tenant_id, file_id))
            };
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
                Some(bucket),
                storage_path.clone(),
                sz,
                hr_auto_approve,
            )
            .await;
            if inserted.is_err() {
                s3_delete(&op, &storage_path).await;
            }
            inserted
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
    let document = employee_document::Entity::find_by_id(document_id)
        .filter(employee_document::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "employeeDocument",
            id: document_id.to_string(),
        })?;
    let stored_file = match document.file_storage_id {
        Some(file_id) => file_storage::Entity::find_by_id(file_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .one(db)
            .await?,
        None => None,
    };

    let txn = db.begin().await?;
    employee_document::Entity::delete_by_id(document.id).exec(&txn).await?;
    if let Some(file_id) = document.file_storage_id {
        file_storage::Entity::delete_by_id(file_id).exec(&txn).await?;
    }
    txn.commit().await?;

    if let Some(stored_file) = stored_file {
        if stored_file.provider == PROVIDER_LOCAL {
            let root = local_file_root();
            let full_path = root.join(&stored_file.storage_path);
            if full_path.starts_with(&root) {
                let _ = tokio::fs::remove_file(full_path).await;
            }
        } else if stored_file.provider == PROVIDER_S3_COMPAT {
            if let (Ok(settings), Some(bucket)) = (S3CompatSettings::from_env(), stored_file.bucket) {
                if let Ok(operator) = s3_operator_for_bucket(&settings, &bucket) {
                    s3_delete(&operator, &stored_file.storage_path).await;
                }
            }
        }
    }
    Ok(())
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
            let rel = format!("{}/{}", tenant_id, file_id);
            let path = absolute_storage_path(tenant_id, file_id);
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
                rel,
                bytes.len() as i64,
            )
            .await;
            if inserted.is_err() {
                let _ = tokio::fs::remove_file(&path).await;
            }
            inserted
        }
        FileStorageMode::S3Compat => {
            let cfg = S3CompatSettings::from_env()?;
            let file_id = Uuid::new_v4();
            let now = Utc::now();
            let (bucket, storage_path): (String, String) = if cfg.per_tenant_bucket {
                let b = tenant_bucket_name(tenant_id, &cfg.bucket_prefix);
                ensure_tenant_bucket(&cfg, &b).await?;
                (b, file_id.to_string())
            } else {
                let b = cfg
                    .default_bucket
                    .as_ref()
                    .expect("validated in S3CompatSettings::from_env")
                    .clone();
                ensure_tenant_bucket(&cfg, &b).await?;
                (b, format!("{}/{}", tenant_id, file_id))
            };
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
                Some(bucket),
                storage_path.clone(),
                size,
            )
            .await;
            if inserted.is_err() {
                s3_delete(&op, &storage_path).await;
            }
            inserted
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
    let rel = format!("{}/{}", tenant_id, file_id);
    let path = absolute_storage_path(tenant_id, file_id);
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
        rel,
        sz,
        hr_auto_approve,
    )
    .await;
    if inserted.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    inserted
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

/// Build signed claims for a short-TTL GET URL (no DB required on download).
pub fn download_claims(
    tenant_id: Uuid,
    file_storage_id: Uuid,
    mime_type: Option<String>,
    ttl_seconds: i64,
) -> kabipay_common::file_download_token::FileDownloadClaims {
    file_download_claims(tenant_id, file_storage_id, mime_type, ttl_seconds)
}

/// Build a time-limited HMAC download URL (HTTP GET) for a stored file.
pub fn public_download_url(
    claims: &kabipay_common::file_download_token::FileDownloadClaims,
) -> KabiPayResult<String> {
    public_employee_file_download_url(claims)
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
