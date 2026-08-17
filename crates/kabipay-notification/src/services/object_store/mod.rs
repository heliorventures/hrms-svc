//! Pluggable file backends (LOCAL, S3-compatible). Duplicated from kabipay-employee for tenant blob writes.

mod config;
mod s3_tenant;

pub use config::FileStorageMode;
pub use config::S3CompatSettings;
pub use s3_tenant::ensure_tenant_bucket;
pub use s3_tenant::s3_delete;
pub use s3_tenant::s3_operator_for_bucket;
pub use s3_tenant::s3_put;
pub use s3_tenant::s3_read;
pub use s3_tenant::tenant_bucket_name;
pub use s3_tenant::PROVIDER_S3_COMPAT;
