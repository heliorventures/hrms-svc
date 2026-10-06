//! Storage readers shared by services after resource authorization.

use crate::object_store_config::S3CompatSettings;
use crate::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use opendal::{services::S3, Operator};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::path::{Component, Path, PathBuf};
use tokio::io::AsyncReadExt;

const PROVIDER_LOCAL: &str = "LOCAL";
const PROVIDER_S3_COMPAT: &str = "S3";
const MAX_FILE_BYTES: u64 = 6 * 1024 * 1024;

pub fn local_file_root() -> PathBuf {
    let root =
        std::env::var("KABIPAY_LOCAL_FILE_ROOT").unwrap_or_else(|_| "data/tenant_files".into());
    PathBuf::from(root)
}

pub fn s3_operator_for_bucket(cfg: &S3CompatSettings, bucket: &str) -> KabiPayResult<Operator> {
    let mut s3 = S3::default();
    s3 = s3
        .bucket(bucket)
        .endpoint(cfg.endpoint.as_str())
        .region(cfg.region.as_str())
        .access_key_id(cfg.access_key_id.as_str())
        .secret_access_key(cfg.secret_access_key.as_str())
        .root("/");
    if !cfg.path_style {
        s3 = s3.enable_virtual_host_style();
    }
    Operator::new(s3)
        .map_err(|e| KabiPayError::Internal(format!("S3 operator: {e}")))
        .map(|b| b.finish())
}

pub async fn read_stored_file_bytes(
    db: &DatabaseConnection,
    file_root: &Path,
    row: &file_storage::Model,
) -> KabiPayResult<Vec<u8>> {
    let bytes = read_file_bytes(db, file_root, row).await?;
    if bytes.len() as u64 > MAX_FILE_BYTES {
        return Err(KabiPayError::Validation(
            "file exceeds the 6 MB download limit".into(),
        ));
    }
    Ok(bytes)
}

async fn read_file_bytes(
    db: &DatabaseConnection,
    file_root: &Path,
    row: &file_storage::Model,
) -> KabiPayResult<Vec<u8>> {
    if row.provider == "DATABASE" {
        use kabipay_db_entities::tenant::d0080_prejoining::prejoining_document;
        return prejoining_document::Entity::find()
            .filter(prejoining_document::Column::TenantId.eq(row.tenant_id))
            .filter(prejoining_document::Column::FileStorageId.eq(row.id))
            .one(db)
            .await?
            .map(|document| document.bytes)
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "document",
                id: "requested".into(),
            });
    }
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
        let root = tokio::fs::canonicalize(file_root)
            .await
            .map_err(read_error)?;
        let full = tokio::fs::canonicalize(root.join(&row.storage_path))
            .await
            .map_err(read_error)?;
        if !full.starts_with(&root) {
            return Err(KabiPayError::Validation("invalid file path".into()));
        }
        let file = tokio::fs::File::open(full).await.map_err(read_error)?;
        let mut bytes = Vec::new();
        file.take(MAX_FILE_BYTES + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(read_error)?;
        return Ok(bytes);
    }
    if row.provider == PROVIDER_S3_COMPAT {
        let cfg = S3CompatSettings::from_env()?;
        let b = row
            .bucket
            .as_ref()
            .ok_or_else(|| KabiPayError::Internal("S3 file missing bucket name in DB".into()))?;
        let op = s3_operator_for_bucket(&cfg, b)?;
        return op
            .read_with(&row.storage_path)
            .range(0..MAX_FILE_BYTES + 1)
            .await
            .map(|bytes| bytes.to_vec())
            .map_err(|_| KabiPayError::Internal("supporting file could not be read".into()));
    }
    Err(KabiPayError::Validation(
        "unsupported file storage provider".into(),
    ))
}

fn read_error(error: std::io::Error) -> KabiPayError {
    if error.kind() == std::io::ErrorKind::NotFound {
        KabiPayError::NotFound {
            entity: "document",
            id: "requested".into(),
        }
    } else {
        KabiPayError::Internal("file could not be read".into())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::Utc;
    use uuid::Uuid;

    fn file_row(path: &str) -> file_storage::Model {
        file_storage::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            provider: "LOCAL".into(),
            bucket: None,
            storage_path: path.into(),
            original_filename: Some("evidence.pdf".into()),
            mime_type: Some("application/pdf".into()),
            file_size_bytes: Some(3),
            is_public: false,
            uploaded_by: Some(Uuid::new_v4()),
            created_at: Utc::now(),
            updated_at: Utc::now(),
        }
    }

    #[tokio::test]
    async fn private_file_reader_rejects_traversal_and_redacts_missing_paths() {
        let db = DatabaseConnection::Disconnected;
        for path in [
            "../private.pdf",
            "/private.pdf",
            "folder\\private.pdf",
            "C:\\private.pdf",
        ] {
            assert!(matches!(
                read_stored_file_bytes(&db, Path::new("unused"), &file_row(path)).await,
                Err(KabiPayError::Validation(_))
            ));
        }
        let error = read_stored_file_bytes(
            &db,
            Path::new("a-nonexistent-private-directory"),
            &file_row("evidence.pdf"),
        )
        .await
        .unwrap_err();
        assert!(!error
            .to_string()
            .contains("a-nonexistent-private-directory"));
    }

    #[tokio::test]
    async fn private_file_reader_bounds_actual_content_and_rejects_unknown_provider() {
        let root = std::env::temp_dir().join(format!("hrms-reader-{}", Uuid::new_v4()));
        tokio::fs::create_dir(&root).await.unwrap();
        let path = root.join("evidence.pdf");
        let db = DatabaseConnection::Disconnected;
        tokio::fs::write(&path, b"pdf").await.unwrap();
        assert_eq!(
            read_stored_file_bytes(&db, &root, &file_row("evidence.pdf"))
                .await
                .unwrap(),
            b"pdf"
        );
        tokio::fs::write(&path, vec![0; MAX_FILE_BYTES as usize + 2])
            .await
            .unwrap();
        assert!(matches!(
            read_stored_file_bytes(&db, &root, &file_row("evidence.pdf")).await,
            Err(KabiPayError::Validation(_))
        ));
        let mut unsupported = file_row("evidence.pdf");
        unsupported.provider = "OTHER".into();
        assert!(matches!(
            read_stored_file_bytes(&db, &root, &unsupported).await,
            Err(KabiPayError::Validation(_))
        ));
        tokio::fs::remove_file(path).await.unwrap();
        tokio::fs::remove_dir(root).await.unwrap();
    }
}
