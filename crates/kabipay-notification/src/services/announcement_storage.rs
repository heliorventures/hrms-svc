//! `file_storage` rows for announcement attachments (no `employee_document`).

use std::path::{Component, PathBuf};

use chrono::Utc;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use sea_orm::{ActiveModelTrait, ActiveValue::Set, DatabaseConnection};
use uuid::Uuid;

use super::object_store::{
    FileStorageMode, S3CompatSettings, PROVIDER_S3_COMPAT, ensure_tenant_bucket, s3_delete,
    s3_operator_for_bucket, s3_put, s3_read, tenant_bucket_name,
};

const PROVIDER_LOCAL: &str = "LOCAL";
const MAX_BYTES: usize = 6 * 1024 * 1024;
const ALLOWED_ANNOUNCEMENT_MIME_TYPES: &[&str] = &[
    "application/pdf",
    "application/msword",
    "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    "image/gif",
    "image/jpeg",
    "image/png",
    "image/webp",
    "text/plain",
];

pub fn local_file_root() -> PathBuf {
    let root =
        std::env::var("KABIPAY_LOCAL_FILE_ROOT").unwrap_or_else(|_| "data/tenant_files".into());
    PathBuf::from(root)
}

fn local_fallback_enabled() -> bool {
    // Keep announcement attachments on the same storage policy as employee/company documents:
    // object storage is preferred, while local tenant-scoped storage is the default recovery path.
    if let Ok(value) = std::env::var("KABIPAY_FILE_STORAGE_FALLBACK") {
        return !matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "0" | "false" | "no" | "none" | "disabled" | "off"
        );
    }
    if let Ok(value) = std::env::var("KABIPAY_FILE_STORAGE_LOCAL_FALLBACK") {
        return matches!(
            value.trim().to_ascii_lowercase().as_str(),
            "1" | "true" | "yes" | "local" | "disk"
        );
    }
    true
}

/// Read announcement attachment bytes through the private storage backend.
pub async fn read_blob(row: &file_storage::Model) -> KabiPayResult<Vec<u8>> {
    match row.provider.trim().to_ascii_uppercase().as_str() {
        PROVIDER_LOCAL => read_local_blob(row).await,
        PROVIDER_S3_COMPAT => read_s3_blob(row).await,
        provider => Err(KabiPayError::Validation(format!(
            "unsupported file storage provider: {provider}"
        ))),
    }
}

async fn read_local_blob(row: &file_storage::Model) -> KabiPayResult<Vec<u8>> {
    if row
        .storage_path
        .as_str()
        .contains('\\')
        || std::path::Path::new(&row.storage_path)
            .components()
            .any(|part| {
                matches!(
                    part,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
    {
        return Err(KabiPayError::Validation("invalid file path".into()));
    }
    let file_root = local_file_root();
    let full = file_root.join(&row.storage_path);
    tokio::fs::read(&full)
        .await
        .map_err(|e| KabiPayError::Internal(format!("read local file: {e}")))
}

async fn read_s3_blob(row: &file_storage::Model) -> KabiPayResult<Vec<u8>> {
    let cfg = S3CompatSettings::from_env()?;
    let bucket = row
        .bucket
        .clone()
        .or_else(|| cfg.default_bucket.clone())
        .ok_or_else(|| KabiPayError::Validation("file storage bucket is missing".into()))?;
    let op = s3_operator_for_bucket(&cfg, &bucket)?;
    s3_read(&op, &row.storage_path).await
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
        "attachment".to_string()
    } else {
        sanitized
    }
}

fn storage_path_for(
    tenant_id: Uuid,
    uploaded_by: Option<Uuid>,
    file_id: Uuid,
    original_filename: &str,
) -> String {
    let owner = uploaded_by
        .map(|id| id.to_string())
        .unwrap_or_else(|| "unassigned".to_string());
    format!(
        "tenants/{tenant_id}/users/{owner}/announcements/{file_id}/{}",
        safe_filename(original_filename)
    )
}

fn absolute_storage_path(storage_path: &str) -> KabiPayResult<PathBuf> {
    if storage_path.contains('\\')
        || std::path::Path::new(storage_path)
            .components()
            .any(|part| {
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

/// Persist bytes to disk or object storage; returns new `file_storage.id`.
pub async fn store_blob(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploaded_by: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<Uuid> {
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
    validate_supported_upload_type(&mime_type, &bytes)?;

    let mode = FileStorageMode::from_env();
    match mode {
        FileStorageMode::Local => {
            upload_local(
                db,
                tenant_id,
                uploaded_by,
                original_filename,
                mime_type,
                bytes,
            )
            .await
        }
        FileStorageMode::S3Compat => {
            let s3_result = upload_s3(
                db,
                tenant_id,
                uploaded_by,
                original_filename.clone(),
                mime_type.clone(),
                bytes.clone(),
            )
            .await;
            match s3_result {
                Ok(id) => Ok(id),
                Err(error) if local_fallback_enabled() => {
                    tracing::warn!(
                        error = %error,
                        tenant_id = %tenant_id,
                        "S3 announcement attachment upload failed; using configured local fallback"
                    );
                    upload_local(db, tenant_id, uploaded_by, original_filename, mime_type, bytes)
                        .await
                }
                Err(error) => Err(error),
            }
        }
        FileStorageMode::AzureBlob => Err(KabiPayError::Validation(
            "KABIPAY_FILE_STORAGE_MODE=azure is not implemented yet.".into(),
        )),
    }
}

async fn upload_local(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploaded_by: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<Uuid> {
    let file_id = Uuid::new_v4();
    let now = Utc::now();
    let rel = storage_path_for(tenant_id, uploaded_by, file_id, &original_filename);
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
    let inserted = insert_fs_row(
        db,
        tenant_id,
        uploaded_by,
        original_filename,
        mime_type,
        file_id,
        now,
        None,
        rel,
        sz,
    )
    .await;
    if inserted.is_err() {
        let _ = tokio::fs::remove_file(&path).await;
    }
    inserted
}

async fn upload_s3(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploaded_by: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    bytes: Vec<u8>,
) -> KabiPayResult<Uuid> {
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
    let storage_path = storage_path_for(tenant_id, uploaded_by, file_id, &original_filename);
    let sz = bytes.len() as i64;
    let op = s3_operator_for_bucket(&cfg, &bucket)?;
    s3_put(
        &op,
        &storage_path,
        bytes,
        mime_type.as_deref().filter(|s| !s.is_empty()),
    )
    .await?;
    let inserted = insert_fs_row(
        db,
        tenant_id,
        uploaded_by,
        original_filename,
        mime_type,
        file_id,
        now,
        Some(bucket),
        storage_path.clone(),
        sz,
    )
    .await;
    if inserted.is_err() {
        s3_delete(&op, &storage_path).await;
    }
    inserted
}

fn validate_supported_upload_type(mime_type: &Option<String>, bytes: &[u8]) -> KabiPayResult<()> {
    let mime = mime_type
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !ALLOWED_ANNOUNCEMENT_MIME_TYPES.contains(&mime.as_str()) {
        return Err(KabiPayError::Validation(
            "announcement attachment file type is not allowed".into(),
        ));
    }
    let matches_magic = match mime.as_str() {
        "application/pdf" => bytes.starts_with(b"%PDF-"),
        "application/msword" => bytes.starts_with(&[0xD0, 0xCF, 0x11, 0xE0]),
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => {
            bytes.starts_with(b"PK\x03\x04")
        }
        "image/gif" => bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a"),
        "image/jpeg" => bytes.starts_with(&[0xFF, 0xD8, 0xFF]),
        "image/png" => bytes.starts_with(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]),
        "image/webp" => bytes.len() >= 12 && &bytes[0..4] == b"RIFF" && &bytes[8..12] == b"WEBP",
        "text/plain" => std::str::from_utf8(bytes).is_ok(),
        _ => false,
    };
    if !matches_magic {
        return Err(KabiPayError::Validation(
            "announcement attachment content does not match its declared file type".into(),
        ));
    }
    Ok(())
}

async fn insert_fs_row(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    uploaded_by: Option<Uuid>,
    original_filename: String,
    mime_type: Option<String>,
    file_id: Uuid,
    now: chrono::DateTime<Utc>,
    bucket: Option<String>,
    storage_path: String,
    size: i64,
) -> KabiPayResult<Uuid> {
    let provider = if bucket.is_some() {
        PROVIDER_S3_COMPAT.into()
    } else {
        PROVIDER_LOCAL.into()
    };
    let am = file_storage::ActiveModel {
        id: Set(file_id),
        tenant_id: Set(tenant_id),
        provider: Set(provider),
        bucket: Set(bucket),
        storage_path: Set(storage_path),
        original_filename: Set(Some(original_filename)),
        mime_type: Set(mime_type),
        file_size_bytes: Set(Some(size)),
        is_public: Set(false),
        uploaded_by: Set(uploaded_by),
        created_at: Set(now),
        updated_at: Set(now),
    };
    am.insert(db).await.map_err(KabiPayError::from)?;
    Ok(file_id)
}
