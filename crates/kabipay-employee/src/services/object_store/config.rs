//! Environment-driven storage configuration (no hardcoded provider endpoints or keys).

pub use kabipay_common::object_store_config::S3CompatSettings;

/// Top-level file storage mode. Extend with new variants when adding Azure, GCS, etc.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileStorageMode {
    Local,
    /// Any S3 API–compatible object store: AWS S3, Cloudflare R2, MinIO, Ceph, …
    S3Compat,
    /// Placeholder for `services Azblob` / OpenDAL `azblob` (not implemented yet)
    #[allow(dead_code)]
    AzureBlob,
}

impl FileStorageMode {
    pub fn from_env() -> Self {
        let raw = std::env::var("KABIPAY_FILE_STORAGE_MODE")
            .unwrap_or_else(|_| "local".into());
        let s = raw.trim().to_ascii_lowercase();
        match s.as_str() {
            "local" | "disk" => FileStorageMode::Local,
            "s3_compat" | "s3" | "r2" | "minio" => FileStorageMode::S3Compat,
            "azure" | "azure_blob" | "azblob" => FileStorageMode::AzureBlob,
            _ => FileStorageMode::Local,
        }
    }
}
