//! Pluggable file backends. Today: `LOCAL`, S3-compatible (`KABIPAY_FILE_STORAGE_MODE=s3_compat` — R2, AWS, MinIO).  
//! **Tenant** comes from the GraphQL request (JWT + DB); the store **ensures** a namespace: **per-tenant S3 bucket**
//! (default) or **shared bucket** with key prefix `tenant_id/file_id` — see [`S3CompatSettings`].
//! `AzureBlob` is reserved; implement with OpenDAL `azblob` when needed.

mod config;
mod s3_tenant;

pub use config::FileStorageMode;
pub use config::S3CompatSettings;
pub use s3_tenant::ensure_tenant_bucket;
pub use s3_tenant::s3_operator_for_bucket;
pub use s3_tenant::s3_put;
pub use s3_tenant::s3_read;
pub use s3_tenant::tenant_bucket_name;
pub use s3_tenant::PROVIDER_S3_COMPAT;
