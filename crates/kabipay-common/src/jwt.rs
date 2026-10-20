//! JWT encoding / decoding primitives.
//!
//! Two issuers — never interchangeable:
//!   - `kabipay-ops`    → operator plane
//!   - `kabipay-client` → tenant plane
//!
//! These explicit-secret primitives support legacy deployments and fixtures.
//! Production client callers use `client_jwt`, which supports verification-only
//! company-bound RSA keys. Organization company runtimes require RSA mode.

use crate::context::{ClientClaims, OperatorClaims, CLIENT_JWT_ISSUER, OPERATOR_JWT_ISSUER};
use crate::error::{KabiPayError, KabiPayResult};
use chrono::{Duration, Utc};
use jsonwebtoken::{decode, encode, DecodingKey, EncodingKey, Header, Validation};

/// Encode an operator JWT.
pub fn encode_operator_jwt(claims: &OperatorClaims, secret: &[u8]) -> KabiPayResult<String> {
    let key = EncodingKey::from_secret(secret);
    encode(&Header::default(), claims, &key).map_err(Into::into)
}

/// Encode a client JWT.
pub fn encode_client_jwt(claims: &ClientClaims, secret: &[u8]) -> KabiPayResult<String> {
    let key = EncodingKey::from_secret(secret);
    encode(&Header::default(), claims, &key).map_err(Into::into)
}

/// Decode and validate an operator JWT. Rejects tokens with a non-`kabipay-ops` issuer.
pub fn decode_operator_jwt(token: &str, secret: &[u8]) -> KabiPayResult<OperatorClaims> {
    let mut validation = Validation::default();
    validation.set_issuer(&[OPERATOR_JWT_ISSUER]);
    validation.leeway = 30;
    let key = DecodingKey::from_secret(secret);
    Ok(decode::<OperatorClaims>(token, &key, &validation)?.claims)
}

/// Decode and validate a client JWT. Rejects tokens with a non-`kabipay-client` issuer.
pub fn decode_client_jwt(token: &str, secret: &[u8]) -> KabiPayResult<ClientClaims> {
    let mut validation = Validation::default();
    validation.set_issuer(&[CLIENT_JWT_ISSUER]);
    validation.leeway = 30;
    let key = DecodingKey::from_secret(secret);
    Ok(decode::<ClientClaims>(token, &key, &validation)?.claims)
}

/// Build expiry timestamps consistently.
pub fn expiry_timestamp(ttl_hours: i64) -> i64 {
    (Utc::now() + Duration::hours(ttl_hours)).timestamp()
}

pub fn issued_at_timestamp() -> i64 {
    Utc::now().timestamp()
}

/// Extract the bearer token from `Authorization: Bearer <token>`.
pub fn extract_bearer(auth_header: &str) -> KabiPayResult<&str> {
    auth_header
        .strip_prefix("Bearer ")
        .ok_or(KabiPayError::Unauthorised)
}

/// Read the shared JWT secret from the process environment. Falls back to
/// a well-known dev secret so local `cargo run` keeps working — MUST be
/// overridden in production via `KABIPAY_JWT_SECRET`.
pub fn jwt_secret_from_env() -> Vec<u8> {
    std::env::var("KABIPAY_JWT_SECRET")
        .unwrap_or_else(|_| "dev-only-change-me-in-prod".to_string())
        .into_bytes()
}
