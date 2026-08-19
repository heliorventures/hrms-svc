//! S3 / R2 / MinIO: per-tenant bucket + object keys, via OpenDAL + signed CreateBucket.

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

/// S3–compatible label for `file_storage.provider` (R2, AWS, MinIO, …).
pub const PROVIDER_S3_COMPAT: &str = "S3";

/// Per-tenant bucket: `{prefix}-{uuid-without-dashes}`. Lowercase, S3 / R2–safe.
pub fn tenant_bucket_name(tenant_id: Uuid, prefix: &str) -> String {
    format!("{}-{}", prefix, tenant_id.as_simple())
}

/// Build an OpenDAL operator for a single named bucket.
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

/// Create bucket on the remote if not seen in-process yet (R2: same as S3 CreateBucket).
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
    let uri = url.parse().map_err(|e| {
        KabiPayError::Internal(format!("KABIPAY_S3_ENDPOINT + bucket: invalid URL: {e}"))
    })?;

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
        // 200 OK, or 409 if bucket already exists
        return Ok(());
    }
    let text = resp.text().await.unwrap_or_default();
    Err(KabiPayError::Internal(format!(
        "S3 create bucket: HTTP {st} {text}"
    )))
}

/// Put an object; `key` is relative to the bucket (no leading slash in DB).
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

/// Read an object in full (fits employee doc max size).
pub async fn s3_read(op: &Operator, key: &str) -> KabiPayResult<Vec<u8>> {
    let b = op
        .read(key)
        .await
        .map_err(|e| KabiPayError::Internal(format!("S3 get_object: {e}")))?;
    let bytes = b.to_bytes();
    Ok(bytes.to_vec())
}

#[cfg(test)]
mod s3_r2_env_tests {
    use super::*;
    use super::super::config::S3CompatSettings;
    use uuid::Uuid;

    /// Real `CreateBucket` (S3 `PUT` to `endpoint/bucket`). R2 may return **403** if the
    /// S3 API token is scoped to object read/write on existing buckets only — then create
    /// buckets in the Cloudflare dashboard, or use an R2 / Account token with bucket creation.
    ///
    /// `cargo test s3_create_bucket_uses_env -p kabipay-employee -- --ignored --nocapture`
    #[tokio::test]
    #[ignore]
    async fn s3_create_bucket_uses_env() {
        kabipay_common::load_dotenv();
        let cfg = S3CompatSettings::from_env().expect("set KABIPAY_S3_ENDPOINT, keys, and region in .env");
        let bucket = format!("kabipay-smk-{}", Uuid::new_v4().as_simple());
        ensure_tenant_bucket(&cfg, &bucket)
            .await
            .expect("CreateBucket (PUT) — 403: token may not allow new buckets; see test doc comment");
        eprintln!("CreateBucket ok: {bucket}");
    }

    /// **Put + Get** in a bucket. If `KABIPAY_S3_SMOKE_TEST_BUCKET` is set, uses that bucket
    /// (create it in the R2 **dashboard** first). Otherwise creates a throwaway bucket
    /// (requires create permission).
    #[tokio::test]
    #[ignore]
    async fn s3_put_get_round_trip_uses_env() {
        kabipay_common::load_dotenv();
        let cfg = S3CompatSettings::from_env().expect("KABIPAY_S3_* in .env");
        let bucket = match std::env::var("KABIPAY_S3_SMOKE_TEST_BUCKET")
            .ok()
            .and_then(|s| {
                let t = s.trim();
                if t.is_empty() {
                    None
                } else {
                    Some(t.to_string())
                }
            }) {
            Some(b) => b,
            None => {
                let b = format!("kabipay-smk-{}", Uuid::new_v4().as_simple());
                ensure_tenant_bucket(&cfg, &b)
                    .await
                    .expect("CreateBucket — or set KABIPAY_S3_SMOKE_TEST_BUCKET to a pre-created bucket");
                b
            }
        };
        let op = s3_operator_for_bucket(&cfg, &bucket).expect("operator");
        let key = "smoke-test.txt";
        let payload = b"ok";
        s3_put(
            &op,
            key,
            payload.to_vec(),
            Some("text/plain"),
        )
        .await
        .expect("put");
        let got = s3_read(&op, key).await.expect("get");
        assert_eq!(got, payload);
        eprintln!("Put/Get ok in bucket {bucket} key {key}");
    }
}
