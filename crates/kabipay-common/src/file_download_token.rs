//! HMAC-signed, short-TTL tokens for unauthenticated HTTP GET file downloads
//! (same secret as JWT in dev; split in production if needed).

use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

use crate::jwt::jwt_secret_from_env;
use crate::{KabiPayError, KabiPayResult};

type HmacSha256 = Hmac<Sha256>;

#[derive(Debug, Serialize, Deserialize)]
pub struct FileDownloadClaims {
    pub tenant_id: Uuid,
    pub file_storage_id: Uuid,
    pub exp: i64,
    #[serde(default)]
    pub mime_type: Option<String>,
}

/// Build `payload_b64.hmac_b64` wire token.
pub fn sign_download_token(claims: &FileDownloadClaims) -> String {
    let json = serde_json::to_string(claims).expect("serialize file claims");
    let mut mac =
        HmacSha256::new_from_slice(&jwt_secret_from_env()).expect("HMAC can take key of any size");
    mac.update(json.as_bytes());
    let tag = mac.finalize().into_bytes();
    let p = URL_SAFE_NO_PAD.encode(json);
    let s = URL_SAFE_NO_PAD.encode(tag);
    format!("{p}.{s}")
}

/// Verify signature and return claims if not expired.
pub fn verify_download_token(token: &str) -> Option<FileDownloadClaims> {
    let (p, s) = token.split_once('.')?;
    let json = String::from_utf8(URL_SAFE_NO_PAD.decode(p).ok()?).ok()?;
    let tag = URL_SAFE_NO_PAD.decode(s).ok()?;
    let mut mac = HmacSha256::new_from_slice(&jwt_secret_from_env()).ok()?;
    mac.update(json.as_bytes());
    mac.verify_slice(&tag).ok()?;
    let c: FileDownloadClaims = serde_json::from_str(&json).ok()?;
    if c.exp < chrono::Utc::now().timestamp() {
        return None;
    }
    Some(c)
}

/// Claims for a time-limited download (used by GraphQL resolvers before signing).
pub fn file_download_claims(
    tenant_id: Uuid,
    file_storage_id: Uuid,
    mime_type: Option<String>,
    ttl_seconds: i64,
) -> FileDownloadClaims {
    FileDownloadClaims {
        tenant_id,
        file_storage_id,
        exp: chrono::Utc::now().timestamp() + ttl_seconds,
        mime_type,
    }
}

fn allow_local_download_base() -> bool {
    cfg!(debug_assertions)
        || std::env::var("KABIPAY_ALLOW_LOCAL_FILE_DOWNLOAD_BASE")
            .map(|value| value.trim().eq_ignore_ascii_case("true"))
            .unwrap_or(false)
}

fn host_from_http_base(base: &str) -> Option<&str> {
    let after_scheme = base
        .strip_prefix("http://")
        .or_else(|| base.strip_prefix("https://"))?;
    let authority = after_scheme.split('/').next().unwrap_or_default();
    let host = authority
        .strip_prefix('[')
        .and_then(|rest| rest.split_once(']').map(|(host, _)| host))
        .unwrap_or_else(|| authority.split(':').next().unwrap_or_default());
    if host.is_empty() {
        None
    } else {
        Some(host)
    }
}

fn is_loopback_or_wildcard_host(host: &str) -> bool {
    let lower = host.trim().to_ascii_lowercase();
    lower == "localhost" || lower == "::1" || lower == "0.0.0.0" || lower.starts_with("127.")
}

pub fn public_employee_file_download_base_from_env() -> KabiPayResult<String> {
    let base = std::env::var("KABIPAY_EMPLOYEE_PUBLIC_BASE").map_err(|_| {
        KabiPayError::Validation(
            "KABIPAY_EMPLOYEE_PUBLIC_BASE must be set to the public employee service URL".into(),
        )
    })?;
    let base = base.trim().trim_end_matches('/').to_string();
    if !base.starts_with("http://") && !base.starts_with("https://") {
        return Err(KabiPayError::Validation(
            "KABIPAY_EMPLOYEE_PUBLIC_BASE must start with http:// or https://".into(),
        ));
    }
    let host = host_from_http_base(&base).ok_or_else(|| {
        KabiPayError::Validation("KABIPAY_EMPLOYEE_PUBLIC_BASE must include a host".into())
    })?;
    if is_loopback_or_wildcard_host(host) && !allow_local_download_base() {
        return Err(KabiPayError::Validation(
            "KABIPAY_EMPLOYEE_PUBLIC_BASE cannot use localhost or loopback in production".into(),
        ));
    }
    Ok(base)
}

/// Full URL for `GET /files/employee-document?token=...` on **kabipay-employee**.
pub fn public_employee_file_download_url(claims: &FileDownloadClaims) -> KabiPayResult<String> {
    let base = public_employee_file_download_base_from_env()?;
    let token = sign_download_token(claims);
    let encoded = urlencoding::encode(&token);
    Ok(format!("{base}/files/employee-document?token={encoded}"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn host_parser_reads_http_authority_without_path_or_port() {
        assert_eq!(
            host_from_http_base("https://api.heliorsoft.com/files"),
            Some("api.heliorsoft.com")
        );
        assert_eq!(host_from_http_base("http://127.0.0.1:4013"), Some("127.0.0.1"));
    }

    #[test]
    fn loopback_hosts_are_not_public_download_hosts() {
        assert!(is_loopback_or_wildcard_host("localhost"));
        assert!(is_loopback_or_wildcard_host("127.0.0.1"));
        assert!(is_loopback_or_wildcard_host("0.0.0.0"));
        assert!(!is_loopback_or_wildcard_host("api.heliorsoft.com"));
    }
}
