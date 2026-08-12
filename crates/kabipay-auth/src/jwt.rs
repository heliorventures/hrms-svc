//! JWT access-token issuance and verification.
//!
//! Uses `kabipay_common::jwt` + the shared `ClientClaims` / `OperatorClaims`
//! types so every subgraph's middleware can decode tokens without reaching
//! into this crate.
//!
//! One shared HS256 secret for both planes (plane isolation is enforced by
//! the `iss` claim + separate middleware).

use chrono::{Duration, Utc};
use kabipay_common::{
    context::{ClientClaims, OperatorClaims, CLIENT_JWT_ISSUER, OPERATOR_JWT_ISSUER},
    error::KabiPayResult,
    jwt::{
        decode_client_jwt, decode_operator_jwt, encode_client_jwt, encode_operator_jwt,
        jwt_secret_from_env,
    },
};
use std::collections::HashMap;
use uuid::Uuid;

pub const ISSUER_OPS: &str = OPERATOR_JWT_ISSUER;
pub const ISSUER_CLIENT: &str = CLIENT_JWT_ISSUER;

/// Generic view of the decoded token, used by `/auth/introspect`.
#[derive(Debug, Clone)]
pub struct DecodedToken {
    pub subject: Uuid,
    pub issuer: String,
    pub email: String,
    pub tenant_id: Option<Uuid>,
    pub exp: i64,
}

/// JWT configuration derived from the process environment.
#[derive(Clone, Debug)]
pub struct JwtConfig {
    pub secret: Vec<u8>,
    pub ops_access_ttl_secs: i64,
    pub client_access_ttl_secs: i64,
    pub refresh_ttl_secs: i64,
}

impl JwtConfig {
    /// Reads config from env vars with safe dev-mode defaults.
    /// `KABIPAY_JWT_SECRET` MUST be overridden in production.
    pub fn from_env() -> Self {
        let ops_ttl = std::env::var("KABIPAY_OPS_ACCESS_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(28_800);
        let client_ttl = std::env::var("KABIPAY_CLIENT_ACCESS_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(3_600);
        let refresh_ttl = std::env::var("KABIPAY_REFRESH_TTL_SECS")
            .ok()
            .and_then(|v| v.parse().ok())
            .unwrap_or(2_592_000);
        Self {
            secret: jwt_secret_from_env(),
            ops_access_ttl_secs: ops_ttl,
            client_access_ttl_secs: client_ttl,
            refresh_ttl_secs: refresh_ttl,
        }
    }

    /// Issue an operator-plane access token.
    pub fn issue_ops_access(&self, user_id: Uuid, email: &str) -> KabiPayResult<String> {
        let now = Utc::now();
        let claims = OperatorClaims {
            sub: user_id,
            iss: ISSUER_OPS.into(),
            email: email.into(),
            exp: (now + Duration::seconds(self.ops_access_ttl_secs)).timestamp(),
            iat: now.timestamp(),
            roles: Vec::new(),
            tenant_access: Vec::new(),
        };
        encode_operator_jwt(&claims, &self.secret)
    }

    /// Issue a client-plane access token pinned to `tenant_id`.
    ///
    /// `employee_id` is set when the user is linked to an `employee` row in
    /// the tenant schema (`employee.user_id = user_id`).
    pub fn issue_client_access(
        &self,
        user_id: Uuid,
        tenant_id: Uuid,
        email: &str,
        employee_id: Option<Uuid>,
        must_change_password: bool,
        roles: Vec<String>,
        permissions: Vec<String>,
        resource_scopes: HashMap<String, String>,
    ) -> KabiPayResult<String> {
        let now = Utc::now();
        let claims = ClientClaims {
            sub: user_id,
            iss: ISSUER_CLIENT.into(),
            email: email.into(),
            exp: (now + Duration::seconds(self.client_access_ttl_secs)).timestamp(),
            iat: now.timestamp(),
            tenant_id,
            employee_id,
            must_change_password,
            roles,
            permissions,
            resource_scopes,
        };
        encode_client_jwt(&claims, &self.secret)
    }

    /// Decode either plane's token. Tries ops first then client; returns
    /// whichever validates. Used by `/auth/introspect`.
    pub fn decode_any(&self, token: &str) -> KabiPayResult<DecodedToken> {
        if let Ok(c) = decode_operator_jwt(token, &self.secret) {
            return Ok(DecodedToken {
                subject: c.sub,
                issuer: c.iss,
                email: c.email,
                tenant_id: None,
                exp: c.exp,
            });
        }
        let c = decode_client_jwt(token, &self.secret)?;
        Ok(DecodedToken {
            subject: c.sub,
            issuer: c.iss,
            email: c.email,
            tenant_id: Some(c.tenant_id),
            exp: c.exp,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use kabipay_common::jwt::{decode_client_jwt, decode_operator_jwt};

    fn test_cfg() -> JwtConfig {
        JwtConfig {
            secret: b"test-secret".to_vec(),
            ops_access_ttl_secs: 60,
            client_access_ttl_secs: 60,
            refresh_ttl_secs: 600,
        }
    }

    #[test]
    fn ops_token_round_trip() {
        let cfg = test_cfg();
        let id = Uuid::new_v4();
        let token = cfg.issue_ops_access(id, "ops@example.com").unwrap();
        let claims = decode_operator_jwt(&token, &cfg.secret).unwrap();
        assert_eq!(claims.sub, id);
        assert_eq!(claims.iss, ISSUER_OPS);
        // A client decoder rejects an ops-issued token.
        assert!(decode_client_jwt(&token, &cfg.secret).is_err());
    }

    #[test]
    fn client_token_round_trip() {
        let cfg = test_cfg();
        let user = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let token = cfg
            .issue_client_access(
                user,
                tenant,
                "user@example.com",
                None,
                false,
                vec![],
                vec![],
                HashMap::new(),
            )
            .unwrap();
        let claims = decode_client_jwt(&token, &cfg.secret).unwrap();
        assert_eq!(claims.sub, user);
        assert_eq!(claims.tenant_id, tenant);
        assert_eq!(claims.iss, ISSUER_CLIENT);
    }
}
