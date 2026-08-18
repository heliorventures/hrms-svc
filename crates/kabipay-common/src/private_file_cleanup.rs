//! Durable cleanup for private file objects.
//!
//! Owning services enqueue a tombstone and delete their business/file metadata in one
//! database transaction. The worker claims tombstones, performs idempotent physical deletion,
//! and clears storage coordinates only after deletion succeeds.

use std::path::{Component, Path, PathBuf};

use chrono::{Duration, Utc};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use kabipay_db_entities::tenant::d0059_file_upload_stage::file_upload_stage;
use kabipay_db_entities::tenant::d0060_private_file_cleanup::private_file_cleanup_task;
use opendal::services::S3;
use opendal::Operator;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait,
    QueryFilter, QueryOrder, QuerySelect, Set, TransactionTrait,
};
use sea_orm::sea_query::{LockBehavior, LockType};
use sha2::{Digest, Sha256};
use uuid::Uuid;

use crate::{KabiPayError, KabiPayResult};

const STATUS_PENDING: &str = "PENDING";
const STATUS_PROCESSING: &str = "PROCESSING";
const STATUS_FAILED: &str = "FAILED";
const STATUS_COMPLETED: &str = "COMPLETED";
const PROVIDER_LOCAL: &str = "LOCAL";
const PROVIDER_S3: &str = "S3";
const COMPANY_DOCUMENT_PURPOSE: &str = "COMPANY_DOCUMENT";

struct CleanupCoordinates {
    provider: String,
    bucket: Option<String>,
    storage_path: String,
    local_root: Option<String>,
    deduplication_key: String,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum CleanupErrorClass {
    InvalidStorageMetadata,
    LocalIo,
    StorageConfiguration,
    ObjectStore,
    UnsupportedProvider,
}

impl CleanupErrorClass {
    fn as_str(self) -> &'static str {
        match self {
            Self::InvalidStorageMetadata => "INVALID_STORAGE_METADATA",
            Self::LocalIo => "LOCAL_IO",
            Self::StorageConfiguration => "STORAGE_CONFIGURATION",
            Self::ObjectStore => "OBJECT_STORE",
            Self::UnsupportedProvider => "UNSUPPORTED_PROVIDER",
        }
    }
}

fn configured_local_root() -> PathBuf {
    let configured = std::env::var("KABIPAY_LOCAL_FILE_ROOT")
        .unwrap_or_else(|_| "data/tenant_files".to_string());
    let root = PathBuf::from(configured);
    if root.is_absolute() {
        root
    } else {
        std::env::current_dir()
            .unwrap_or_else(|_| PathBuf::from("."))
            .join(root)
    }
}

fn normalized_configured_local_root() -> KabiPayResult<String> {
    std::fs::canonicalize(configured_local_root())
        .map(|root| root.to_string_lossy().into_owned())
        .map_err(|_| KabiPayError::Internal("private file cleanup coordinates are invalid".into()))
}

fn valid_tenant_storage_path(tenant_id: Uuid, storage_path: &str) -> bool {
    if storage_path.contains('\\') {
        return false;
    }
    let path = Path::new(storage_path);
    if path.components().any(|component| {
        matches!(
            component,
            Component::ParentDir | Component::RootDir | Component::Prefix(_)
        )
    }) {
        return false;
    }
    storage_path.starts_with(&format!("tenants/{tenant_id}/"))
}

fn cleanup_deduplication_key(
    provider: &str,
    bucket: Option<&str>,
    storage_path: &str,
    local_root: Option<&str>,
) -> String {
    let mut hasher = Sha256::new();
    for value in [
        provider,
        bucket.unwrap_or_default(),
        storage_path,
        local_root.unwrap_or_default(),
    ] {
        hasher.update(value.as_bytes());
        hasher.update([0]);
    }

    const LOWER_HEX: &[u8; 16] = b"0123456789abcdef";
    let digest = hasher.finalize();
    let mut encoded = String::with_capacity(digest.len() * 2);
    for byte in digest.iter().copied() {
        encoded.push(LOWER_HEX[(byte >> 4) as usize] as char);
        encoded.push(LOWER_HEX[(byte & 0x0f) as usize] as char);
    }
    encoded
}

/// Validate deletion coordinates before persisting them. This keeps tombstones tenant-bound,
/// makes local paths relative to the configured root, and accepts only the configured S3 shape.
fn validated_cleanup_coordinates(
    tenant_id: Uuid,
    provider: &str,
    bucket: Option<&str>,
    storage_path: &str,
) -> KabiPayResult<CleanupCoordinates> {
    if !valid_tenant_storage_path(tenant_id, storage_path) {
        return Err(KabiPayError::Internal(
            "private file cleanup coordinates are invalid".into(),
        ));
    }

    let (bucket, local_root) = match provider {
        PROVIDER_LOCAL if bucket.is_none() => (
            None,
            Some(normalized_configured_local_root()?),
        ),
        PROVIDER_S3 => (
            Some(
                bucket
                    .filter(|value| !value.trim().is_empty())
                    .ok_or_else(|| {
                        KabiPayError::Internal(
                            "private file cleanup coordinates are invalid".into(),
                        )
                    })?
                    .to_string(),
            ),
            None,
        ),
        _ => {
            return Err(KabiPayError::Internal(
                "private file cleanup coordinates are invalid".into(),
            ));
        }
    };

    let deduplication_key = cleanup_deduplication_key(
        provider,
        bucket.as_deref(),
        storage_path,
        local_root.as_deref(),
    );

    Ok(CleanupCoordinates {
        provider: provider.to_string(),
        bucket,
        storage_path: storage_path.to_string(),
        local_root,
        deduplication_key,
    })
}

async fn enqueue_private_file_cleanup_coordinates_inner<C>(
    db: &C,
    tenant_id: Uuid,
    file_storage_id: Option<Uuid>,
    coordinates: CleanupCoordinates,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    let existing = private_file_cleanup_task::Entity::find()
        .filter(private_file_cleanup_task::Column::TenantId.eq(tenant_id))
        .filter(
            private_file_cleanup_task::Column::DeduplicationKey
                .eq(coordinates.deduplication_key.clone()),
        )
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    if existing.is_some() {
        return Ok(());
    }

    let now = Utc::now();
    private_file_cleanup_task::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        file_storage_id: Set(file_storage_id),
        deduplication_key: Set(coordinates.deduplication_key),
        provider: Set(coordinates.provider),
        bucket: Set(coordinates.bucket),
        storage_path: Set(Some(coordinates.storage_path)),
        local_root: Set(coordinates.local_root),
        status: Set(STATUS_PENDING.to_string()),
        attempt_count: Set(0),
        next_attempt_at: Set(now),
        claimed_at: Set(None),
        last_error_class: Set(None),
        completed_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(KabiPayError::from)?;
    Ok(())
}

/// Persist a coordinate-only tombstone after an object write succeeds but metadata insertion
/// fails. This remains durable even though no `file_storage` row was created.
pub async fn enqueue_private_file_cleanup_coordinates<C>(
    db: &C,
    tenant_id: Uuid,
    provider: &str,
    bucket: Option<&str>,
    storage_path: &str,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    let coordinates = validated_cleanup_coordinates(tenant_id, provider, bucket, storage_path)?;
    enqueue_private_file_cleanup_coordinates_inner(db, tenant_id, None, coordinates).await
}

/// Record the complete deletion coordinates before the `file_storage` row is removed.
/// Callers must invoke this on the same transaction as the owning-reference deletion.
pub async fn enqueue_private_file_cleanup<C>(
    db: &C,
    tenant_id: Uuid,
    file: &file_storage::Model,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    if file.tenant_id != tenant_id || file.is_public {
        return Err(KabiPayError::Internal(
            "private file cleanup ownership invariant failed".into(),
        ));
    }

    let coordinates = validated_cleanup_coordinates(
        tenant_id,
        &file.provider,
        file.bucket.as_deref(),
        &file.storage_path,
    )?;
    enqueue_private_file_cleanup_coordinates_inner(db, tenant_id, Some(file.id), coordinates).await
}

/// Enqueue and remove an unreferenced `file_storage` row atomically.
pub async fn enqueue_and_delete_private_file<C>(
    db: &C,
    tenant_id: Uuid,
    file: &file_storage::Model,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    enqueue_private_file_cleanup(db, tenant_id, file).await?;
    let result = file_storage::Entity::delete_many()
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .filter(file_storage::Column::Id.eq(file.id))
        .exec(db)
        .await
        .map_err(KabiPayError::from)?;
    if result.rows_affected != 1 {
        return Err(KabiPayError::Internal(
            "private file cleanup ownership invariant failed".into(),
        ));
    }
    Ok(())
}

fn retry_delay(attempt_count: i32) -> Duration {
    let exponent = attempt_count.clamp(1, 10) as u32;
    Duration::seconds((5_i64.saturating_mul(2_i64.saturating_pow(exponent))).min(3600))
}

async fn claim_due_task(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Option<private_file_cleanup_task::Model>> {
    let txn = db.begin().await.map_err(KabiPayError::from)?;
    let now = Utc::now();
    let stale_before = now - Duration::minutes(5);

    if let Some(stale) = private_file_cleanup_task::Entity::find()
        .filter(private_file_cleanup_task::Column::TenantId.eq(tenant_id))
        .filter(private_file_cleanup_task::Column::Status.eq(STATUS_PROCESSING))
        .filter(private_file_cleanup_task::Column::ClaimedAt.lt(stale_before))
        .order_by_asc(private_file_cleanup_task::Column::ClaimedAt)
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?
    {
        let mut active: private_file_cleanup_task::ActiveModel = stale.into();
        active.status = Set(STATUS_FAILED.to_string());
        active.claimed_at = Set(None);
        active.last_error_class = Set(Some(CleanupErrorClass::ObjectStore.as_str().to_string()));
        active.next_attempt_at = Set(now);
        active.updated_at = Set(now);
        active.update(&txn).await.map_err(KabiPayError::from)?;
    }

    let row = private_file_cleanup_task::Entity::find()
        .filter(private_file_cleanup_task::Column::TenantId.eq(tenant_id))
        .filter(
            private_file_cleanup_task::Column::Status
                .is_in([STATUS_PENDING.to_string(), STATUS_FAILED.to_string()]),
        )
        .filter(private_file_cleanup_task::Column::NextAttemptAt.lte(now))
        .order_by_asc(private_file_cleanup_task::Column::NextAttemptAt)
        .order_by_asc(private_file_cleanup_task::Column::CreatedAt)
        .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
        .one(&txn)
        .await
        .map_err(KabiPayError::from)?;

    let Some(row) = row else {
        txn.commit().await.map_err(KabiPayError::from)?;
        return Ok(None);
    };
    let mut active: private_file_cleanup_task::ActiveModel = row.clone().into();
    active.status = Set(STATUS_PROCESSING.to_string());
    active.claimed_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(&txn).await.map_err(KabiPayError::from)?;
    txn.commit().await.map_err(KabiPayError::from)?;
    Ok(Some(row))
}

async fn delete_physical_object(
    tenant_id: Uuid,
    task: &private_file_cleanup_task::Model,
) -> Result<(), CleanupErrorClass> {
    let storage_path = task
        .storage_path
        .as_deref()
        .ok_or(CleanupErrorClass::InvalidStorageMetadata)?;
    if !valid_tenant_storage_path(tenant_id, storage_path) {
        return Err(CleanupErrorClass::InvalidStorageMetadata);
    }

    match task.provider.as_str() {
        PROVIDER_LOCAL => {
            let root = task
                .local_root
                .as_deref()
                .map(PathBuf::from)
                .filter(|path| path.is_absolute())
                .ok_or(CleanupErrorClass::InvalidStorageMetadata)?;
            match tokio::fs::remove_file(root.join(storage_path)).await {
                Ok(()) => Ok(()),
                Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(()),
                Err(_) => Err(CleanupErrorClass::LocalIo),
            }
        }
        PROVIDER_S3 => {
            let bucket = task
                .bucket
                .as_deref()
                .filter(|value| !value.trim().is_empty())
                .ok_or(CleanupErrorClass::InvalidStorageMetadata)?;
            let endpoint = std::env::var("KABIPAY_S3_ENDPOINT")
                .ok()
                .filter(|value| value.starts_with("https://") || value.starts_with("http://"))
                .ok_or(CleanupErrorClass::StorageConfiguration)?;
            let region = std::env::var("KABIPAY_S3_REGION").unwrap_or_else(|_| "auto".into());
            let access_key = std::env::var("KABIPAY_S3_ACCESS_KEY_ID")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or(CleanupErrorClass::StorageConfiguration)?;
            let secret_key = std::env::var("KABIPAY_S3_SECRET_ACCESS_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or(CleanupErrorClass::StorageConfiguration)?;
            let path_style = std::env::var("KABIPAY_S3_PATH_STYLE")
                .ok()
                .map(|value| matches!(value.to_ascii_lowercase().as_str(), "1" | "true" | "yes" | "path"))
                .unwrap_or_else(|| endpoint.to_ascii_lowercase().contains("r2.cloudflarestorage.com"));
            let mut builder = S3::default()
                .bucket(bucket)
                .endpoint(&endpoint)
                .region(&region)
                .access_key_id(&access_key)
                .secret_access_key(&secret_key)
                .root("/");
            if !path_style {
                builder = builder.enable_virtual_host_style();
            }
            let operator = Operator::new(builder)
                .map_err(|_| CleanupErrorClass::StorageConfiguration)?
                .finish();
            operator
                .delete(storage_path)
                .await
                .map_err(|_| CleanupErrorClass::ObjectStore)
        }
        _ => Err(CleanupErrorClass::UnsupportedProvider),
    }
}

async fn complete_task(
    db: &DatabaseConnection,
    task: &private_file_cleanup_task::Model,
) -> KabiPayResult<()> {
    let now = Utc::now();
    let mut active: private_file_cleanup_task::ActiveModel = task.clone().into();
    active.status = Set(STATUS_COMPLETED.to_string());
    active.attempt_count = Set(task.attempt_count.saturating_add(1));
    active.bucket = Set(None);
    active.storage_path = Set(None);
    active.local_root = Set(None);
    active.claimed_at = Set(None);
    active.last_error_class = Set(None);
    active.completed_at = Set(Some(now));
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)?;
    Ok(())
}

async fn fail_task(
    db: &DatabaseConnection,
    task: &private_file_cleanup_task::Model,
    error_class: CleanupErrorClass,
) -> KabiPayResult<()> {
    let now = Utc::now();
    let attempts = task.attempt_count.saturating_add(1);
    let mut active: private_file_cleanup_task::ActiveModel = task.clone().into();
    active.status = Set(STATUS_FAILED.to_string());
    active.attempt_count = Set(attempts);
    active.next_attempt_at = Set(now + retry_delay(attempts));
    active.claimed_at = Set(None);
    active.last_error_class = Set(Some(error_class.as_str().to_string()));
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)?;
    Ok(())
}

/// Process up to `limit` cleanup tombstones for one tenant. Logs contain only task IDs and
/// allowlisted failure classes; storage coordinates and raw provider errors are never logged.
pub async fn process_private_file_cleanup_tasks(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: usize,
) -> KabiPayResult<usize> {
    let mut processed = 0;
    for _ in 0..limit.clamp(1, 100) {
        let Some(task) = claim_due_task(db, tenant_id).await? else {
            break;
        };
        match delete_physical_object(tenant_id, &task).await {
            Ok(()) => complete_task(db, &task).await?,
            Err(error_class) => {
                tracing::warn!(
                    tenant_id = %tenant_id,
                    cleanup_task_id = %task.id,
                    error_class = error_class.as_str(),
                    "private file cleanup deferred"
                );
                fail_task(db, &task, error_class).await?;
            }
        }
        processed += 1;
    }
    Ok(processed)
}

/// Durably tombstone expired, unclaimed company-document upload stages. The stage and
/// `file_storage` row disappear only in the transaction that records cleanup coordinates.
pub async fn sweep_expired_company_upload_stages(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: usize,
) -> KabiPayResult<usize> {
    let mut swept = 0;
    for _ in 0..limit.clamp(1, 100) {
        let txn = db.begin().await.map_err(KabiPayError::from)?;
        let stage = file_upload_stage::Entity::find()
            .filter(file_upload_stage::Column::TenantId.eq(tenant_id))
            .filter(file_upload_stage::Column::Purpose.eq(COMPANY_DOCUMENT_PURPOSE))
            .filter(file_upload_stage::Column::ClaimedAt.is_null())
            .filter(file_upload_stage::Column::ExpiresAt.lte(Utc::now()))
            .order_by_asc(file_upload_stage::Column::ExpiresAt)
            .lock_with_behavior(LockType::Update, LockBehavior::SkipLocked)
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?;
        let Some(stage) = stage else {
            txn.commit().await.map_err(KabiPayError::from)?;
            break;
        };
        let file = file_storage::Entity::find_by_id(stage.file_storage_id)
            .filter(file_storage::Column::TenantId.eq(tenant_id))
            .lock_exclusive()
            .one(&txn)
            .await
            .map_err(KabiPayError::from)?;
        if let Some(file) = file {
            enqueue_and_delete_private_file(&txn, tenant_id, &file).await?;
        } else {
            file_upload_stage::Entity::delete_by_id(stage.id)
                .exec(&txn)
                .await
                .map_err(KabiPayError::from)?;
        }
        txn.commit().await.map_err(KabiPayError::from)?;
        swept += 1;
    }
    Ok(swept)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cleanup_paths_are_tenant_bound_and_relative() {
        let tenant_id = Uuid::new_v4();
        assert!(valid_tenant_storage_path(
            tenant_id,
            &format!("tenants/{tenant_id}/users/u/document.pdf")
        ));
        assert!(!valid_tenant_storage_path(tenant_id, "../secret"));
        assert!(!valid_tenant_storage_path(
            tenant_id,
            &format!("tenants/{}/secret", Uuid::new_v4())
        ));
    }

    #[test]
    fn retry_delay_is_bounded() {
        assert!(retry_delay(2) > retry_delay(1));
        assert_eq!(retry_delay(i32::MAX), Duration::hours(1));
    }

    #[test]
    fn local_cleanup_deduplication_includes_the_root_snapshot() {
        let tenant_id = Uuid::new_v4();
        let storage_path = format!("tenants/{tenant_id}/users/u/document.pdf");

        assert_ne!(
            cleanup_deduplication_key(
                PROVIDER_LOCAL,
                None,
                &storage_path,
                Some("/var/lib/kabipay/private-files-a"),
            ),
            cleanup_deduplication_key(
                PROVIDER_LOCAL,
                None,
                &storage_path,
                Some("/var/lib/kabipay/private-files-b"),
            )
        );
    }
}
