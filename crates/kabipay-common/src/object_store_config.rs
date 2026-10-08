//! Shared S3-compatible storage settings.

use crate::{KabiPayError, KabiPayResult};

/// Shared settings for S3-compatible APIs (AWS, R2, MinIO, …).
#[derive(Debug, Clone)]
pub struct S3CompatSettings {
    pub endpoint: String,
    /// AWS region, or `"auto"` for R2
    pub region: String,
    pub access_key_id: String,
    pub secret_access_key: String,
    /// Path-style: `https://host/bucket/key` (typical for R2 account endpoint)
    pub path_style: bool,
    /// Prefix for per-tenant bucket: `{prefix}-{tenant_uuid}`
    pub bucket_prefix: String,
    /// `true` = one bucket per tenant (and create if missing, when we have perms)
    pub per_tenant_bucket: bool,
    /// When `per_tenant_bucket` is `false`, required: shared bucket; keys use `tenant_id/file_id` prefix
    pub default_bucket: Option<String>,
}

impl S3CompatSettings {
    /// Load from env. `KABIPAY_S3_ENDPOINT`, `KABIPAY_S3_ACCESS_KEY_ID`, `KABIPAY_S3_SECRET_ACCESS_KEY` required in `s3_compat` mode.
    pub fn from_env() -> KabiPayResult<Self> {
        let endpoint = std::env::var("KABIPAY_S3_ENDPOINT")
            .map_err(|_| {
                KabiPayError::Validation("KABIPAY_S3_ENDPOINT is required for s3_compat".into())
            })?
            .trim()
            .to_string();
        if endpoint.is_empty() {
            return Err(KabiPayError::Validation(
                "KABIPAY_S3_ENDPOINT is empty".into(),
            ));
        }
        if !endpoint.starts_with("https://") && !endpoint.starts_with("http://") {
            return Err(KabiPayError::Validation(
                "KABIPAY_S3_ENDPOINT must be a full https:// or http:// URL".into(),
            ));
        }
        let access_key_id = std::env::var("KABIPAY_S3_ACCESS_KEY_ID")
            .map_err(|_| {
                KabiPayError::Validation(
                    "KABIPAY_S3_ACCESS_KEY_ID is required for s3_compat".into(),
                )
            })?
            .trim()
            .to_string();
        if access_key_id.is_empty() {
            return Err(KabiPayError::Validation(
                "KABIPAY_S3_ACCESS_KEY_ID is empty".into(),
            ));
        }
        let secret_access_key = std::env::var("KABIPAY_S3_SECRET_ACCESS_KEY")
            .map_err(|_| {
                KabiPayError::Validation(
                    "KABIPAY_S3_SECRET_ACCESS_KEY is required for s3_compat".into(),
                )
            })?
            .trim()
            .to_string();
        if secret_access_key.is_empty() {
            return Err(KabiPayError::Validation(
                "KABIPAY_S3_SECRET_ACCESS_KEY is empty".into(),
            ));
        }

        let region = std::env::var("KABIPAY_S3_REGION").unwrap_or_else(|_| "auto".into());
        let bucket_prefix =
            std::env::var("KABIPAY_S3_BUCKET_PREFIX").unwrap_or_else(|_| "kabipay".into());
        if bucket_prefix.is_empty() {
            return Err(KabiPayError::Validation(
                "KABIPAY_S3_BUCKET_PREFIX is empty".into(),
            ));
        }
        let per_tenant_bucket = !matches!(
            std::env::var("KABIPAY_S3_PER_TENANT_BUCKET")
                .unwrap_or_else(|_| "1".into())
                .to_ascii_lowercase()
                .as_str(),
            "0" | "false" | "no"
        );

        let default_bucket = std::env::var("KABIPAY_S3_DEFAULT_BUCKET")
            .ok()
            .and_then(|s| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            });
        if !per_tenant_bucket && default_bucket.is_none() {
            return Err(KabiPayError::Validation(
                "KABIPAY_S3_DEFAULT_BUCKET is required when KABIPAY_S3_PER_TENANT_BUCKET=0".into(),
            ));
        }

        let path_style = match std::env::var("KABIPAY_S3_PATH_STYLE").ok() {
            None => {
                // R2 account endpoint: path style is the usual interop default
                endpoint
                    .to_ascii_lowercase()
                    .contains("r2.cloudflarestorage.com")
            }
            Some(v) => {
                let v = v.to_ascii_lowercase();
                v == "1" || v == "true" || v == "yes" || v == "path"
            }
        };

        Ok(S3CompatSettings {
            endpoint,
            region,
            access_key_id,
            secret_access_key,
            path_style,
            bucket_prefix,
            per_tenant_bucket,
            default_bucket,
        })
    }
}
