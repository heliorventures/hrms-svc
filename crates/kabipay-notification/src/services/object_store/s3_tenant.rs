//! S3 / R2 / MinIO: per-tenant bucket + object keys.

use std::collections::HashSet;
use std::sync::Mutex;
use std::sync::OnceLock;

use kabipay_common::{KabiPayError, KabiPayResult};
use opendal::services::S3;
use opendal::Operator;
use reqsign::AwsCredential;
use reqsign::AwsV4Signer;
use reqwest::header::HeaderValue;
use uuid::Uuid;

use super::config::S3CompatSettings;

const EMPTY_SHA256: &str =
    "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";

static CREATED_BUCKETS: OnceLock<Mutex<HashSet<String>>> = OnceLock::new();

fn created_bucket_cache() -> &'static Mutex<HashSet<String>> {
    CREATED_BUCKETS.get_or_init(|| Mutex::new(HashSet::new()))
}

pub const PROVIDER_S3_COMPAT: &str = "S3";

pub fn tenant_bucket_name(tenant_id: Uuid, prefix: &str) -> String {
    format!("{}-{}", prefix, tenant_id.as_simple())
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

pub async fn ensure_tenant_bucket(cfg: &S3CompatSettings, bucket: &str) -> KabiPayResult<()> {
    {
        let g = created_bucket_cache();
        if let Ok(s) = g.lock() {
            if s.contains(bucket) {
                return Ok(());
            }
        }
    }
    s3_create_bucket_put(cfg, bucket).await?;
    if let Ok(mut s) = created_bucket_cache().lock() {
        s.insert(bucket.to_string());
    }
    Ok(())
}

async fn s3_create_bucket_put(cfg: &S3CompatSettings, bucket: &str) -> KabiPayResult<()> {
    let base = cfg.endpoint.trim_end_matches('/');
    let url = format!("{base}/{bucket}");
    let uri = url
        .parse()
        .map_err(|e| KabiPayError::Internal(format!("KABIPAY_S3_ENDPOINT + bucket: invalid URL: {e}")))?;

    let mut req = reqwest::Request::new(reqwest::Method::PUT, uri);
    req.headers_mut().insert(
        "x-amz-content-sha256",
        HeaderValue::from_static(EMPTY_SHA256),
    );
    if let Ok(c) = reqwest::header::HeaderValue::from_str("0") {
        req.headers_mut().insert("content-length", c);
    }

    let cred = AwsCredential {
        access_key_id: cfg.access_key_id.clone(),
        secret_access_key: cfg.secret_access_key.clone(),
        session_token: None,
        expires_in: None,
    };
    let signer = AwsV4Signer::new("s3", cfg.region.as_str());
    signer
        .sign(&mut req, &cred)
        .map_err(|e| KabiPayError::Internal(format!("S3 create bucket: sign: {e}")))?;

    let client = reqwest::Client::new();
    let resp = client
        .execute(req)
        .await
        .map_err(|e| KabiPayError::Internal(format!("S3 create bucket: request: {e}")))?;
    let st = resp.status();
    if st.is_success() || st.as_u16() == 409 {
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    Err(KabiPayError::Internal(format!(
        "S3 create bucket: HTTP {st} {text}"
    )))
}

pub async fn s3_put(
    op: &Operator,
    key: &str,
    body: Vec<u8>,
    content_type: Option<&str>,
) -> KabiPayResult<()> {
    if let Some(ct) = content_type.filter(|s| !s.is_empty()) {
        op.write_with(key, body)
            .content_type(ct)
            .await
            .map_err(|e| KabiPayError::Internal(format!("S3 put_object: {e}")))?;
    } else {
        op.write(key, body)
            .await
            .map_err(|e| KabiPayError::Internal(format!("S3 put_object: {e}")))?;
    }
    Ok(())
}

pub async fn s3_read(op: &Operator, key: &str) -> KabiPayResult<Vec<u8>> {
    op.read(key)
        .await
        .map(|buffer| buffer.to_vec())
        .map_err(|e| KabiPayError::Internal(format!("S3 get_object: {e}")))
}

pub async fn s3_delete(op: &Operator, key: &str) -> KabiPayResult<()> {
    op.delete(key)
        .await
        .map_err(|error| KabiPayError::Internal(format!("S3 delete_object: {error}")))
}
