//! Client signing belongs to the bound company's Auth process. Consumers receive
//! only public keys with an explicit company allowlist. RSA mode has no HMAC fallback.
use crate::{
    context::{ClientClaims, CLIENT_JWT_ISSUER},
    KabiPayError, KabiPayResult,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
const TOKEN_TYPE: &str = "hrms-client+jwt";
const AUDIENCE: &str = "kabipay-client-api";
const MAX_LIFETIME: i64 = 86400;
pub struct ClientVerificationKey {
    pub kid: String,
    pub tenant_ids: Vec<Uuid>,
    pub public_pem: Vec<u8>,
}
pub struct ClientJwtVerifier {
    keys: HashMap<String, TrustedKey>,
}
struct TrustedKey {
    tenants: HashSet<Uuid>,
    key: DecodingKey,
}
pub struct ClientJwtIssuer {
    kid: String,
    tenant: Uuid,
    key: EncodingKey,
}
#[derive(Serialize, Deserialize)]
struct Payload {
    aud: String,
    nbf: i64,
    #[serde(flatten)]
    claims: ClientClaims,
}
fn invalid() -> KabiPayError {
    KabiPayError::Internal("invalid client JWT key configuration".into())
}
fn kid_valid(kid: &str) -> bool {
    !kid.is_empty()
        && kid.len() <= 100
        && kid
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
}
fn claims_valid(c: &ClientClaims) -> bool {
    let now = chrono::Utc::now().timestamp();
    c.iss == CLIENT_JWT_ISSUER
        && !c.sub.is_nil()
        && !c.tenant_id.is_nil()
        && c.iat <= now
        && c.exp > now
        && c.exp
            .checked_sub(c.iat)
            .is_some_and(|n| n > 0 && n <= MAX_LIFETIME)
}
impl ClientJwtIssuer {
    pub fn new(kid: &str, tenant: Uuid, pem: &[u8]) -> KabiPayResult<Self> {
        if !kid_valid(kid) || tenant.is_nil() {
            return Err(invalid());
        }
        Ok(Self {
            kid: kid.into(),
            tenant,
            key: EncodingKey::from_rsa_pem(pem).map_err(|_| invalid())?,
        })
    }
    pub fn issue(&self, claims: &ClientClaims) -> KabiPayResult<String> {
        if claims.tenant_id != self.tenant || !claims_valid(claims) {
            return Err(KabiPayError::Unauthorised);
        }
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        header.typ = Some(TOKEN_TYPE.into());
        jsonwebtoken::encode(
            &header,
            &Payload {
                aud: AUDIENCE.into(),
                nbf: claims.iat,
                claims: claims.clone(),
            },
            &self.key,
        )
        .map_err(Into::into)
    }
}
impl ClientJwtVerifier {
    pub fn new(keys: Vec<ClientVerificationKey>) -> KabiPayResult<Self> {
        if keys.is_empty() {
            return Err(invalid());
        }
        let mut parsed = HashMap::new();
        let mut identities = HashMap::new();
        for k in keys {
            let tenants: HashSet<_> = k.tenant_ids.iter().copied().collect();
            if !kid_valid(&k.kid)
                || parsed.contains_key(&k.kid)
                || tenants.len() != 1
                || tenants.contains(&Uuid::nil())
                || tenants.len() != k.tenant_ids.len()
                || k.public_pem.windows(11).any(|p| p == b"PRIVATE KEY")
            {
                return Err(invalid());
            }
            let material: Vec<u8> = k.public_pem.iter().copied().filter(|b| !b.is_ascii_whitespace()).collect();
            if identities.get(&material).is_some_and(|existing| existing != &tenants) {
                return Err(invalid());
            }
            identities.insert(material, tenants.clone());
            parsed.insert(
                k.kid,
                TrustedKey {
                    tenants,
                    key: DecodingKey::from_rsa_pem(&k.public_pem).map_err(|_| invalid())?,
                },
            );
        }
        Ok(Self { keys: parsed })
    }
    pub fn verify(&self, token: &str) -> KabiPayResult<ClientClaims> {
        if token.len() > 16384 {
            return Err(KabiPayError::Unauthorised);
        }
        let h = jsonwebtoken::decode_header(token).map_err(|_| KabiPayError::Unauthorised)?;
        if h.alg != Algorithm::RS256 || h.typ.as_deref() != Some(TOKEN_TYPE) {
            return Err(KabiPayError::Unauthorised);
        }
        let key = self
            .keys
            .get(h.kid.as_deref().unwrap_or_default())
            .ok_or(KabiPayError::Unauthorised)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[CLIENT_JWT_ISSUER]);
        validation.set_audience(&[AUDIENCE]);
        validation.set_required_spec_claims(&["exp", "nbf", "iat", "sub", "iss", "aud"]);
        validation.leeway = 0;
        validation.validate_nbf = true;
        let p = jsonwebtoken::decode::<Payload>(token, &key.key, &validation)
            .map_err(|_| KabiPayError::Unauthorised)?
            .claims;
        if p.nbf != p.claims.iat
            || !claims_valid(&p.claims)
            || !key.tenants.contains(&p.claims.tenant_id)
        {
            return Err(KabiPayError::Unauthorised);
        }
        Ok(p.claims)
    }
}
fn mode() -> KabiPayResult<bool> {
    match std::env::var("KABIPAY_CLIENT_JWT_MODE").as_deref() {
        Ok("rs256") => Ok(true),
        Ok("legacy_hs256") | Err(std::env::VarError::NotPresent) => Ok(false),
        _ => Err(invalid()),
    }
}
fn read_pem(path: &str) -> KabiPayResult<Vec<u8>> {
    let p = std::path::Path::new(path);
    if !p.is_absolute() {
        return Err(invalid());
    }
    let m = std::fs::metadata(p).map_err(|_| invalid())?;
    if !m.is_file() || m.len() > 16384 {
        return Err(invalid());
    }
    std::fs::read(p).map_err(|_| invalid())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct KeyConfig {
    kid: String,
    tenant_ids: Vec<Uuid>,
    public_key_file: String,
}
fn verifier_from_env() -> KabiPayResult<ClientJwtVerifier> {
    let input = std::env::var("KABIPAY_CLIENT_JWT_PUBLIC_KEYS").map_err(|_| invalid())?;
    if input.len() > 65536 {
        return Err(invalid());
    }
    let keys: Vec<KeyConfig> = serde_json::from_str(&input).map_err(|_| invalid())?;
    ClientJwtVerifier::new(
        keys.into_iter()
            .map(|k| {
                Ok(ClientVerificationKey {
                    kid: k.kid,
                    tenant_ids: k.tenant_ids,
                    public_pem: read_pem(&k.public_key_file)?,
                })
            })
            .collect::<KabiPayResult<Vec<_>>>()?,
    )
}
fn issuer_from_env() -> KabiPayResult<ClientJwtIssuer> {
    let kid = std::env::var("KABIPAY_CLIENT_JWT_KEY_ID").map_err(|_| invalid())?;
    let file = std::env::var("KABIPAY_CLIENT_JWT_PRIVATE_KEY_FILE").map_err(|_| invalid())?;
    let tenant = std::env::var("KABIPAY_BOUND_TENANT_ID")
        .ok()
        .and_then(|v| Uuid::parse_str(&v).ok())
        .ok_or_else(invalid)?;
    ClientJwtIssuer::new(&kid, tenant, &read_pem(&file)?)
}
pub fn verify_client_token(token: &str, legacy_secret: &[u8]) -> KabiPayResult<ClientClaims> {
    if mode()? {
        verifier_from_env()?.verify(token)
    } else {
        crate::jwt::decode_client_jwt(token, legacy_secret)
    }
}
pub fn issue_client_token(claims: &ClientClaims, legacy_secret: &[u8]) -> KabiPayResult<String> {
    if mode()? {
        issuer_from_env()?.issue(claims)
    } else {
        crate::jwt::encode_client_jwt(claims, legacy_secret)
    }
}
pub fn validate_client_token_configuration(signing: bool) -> KabiPayResult<()> {
    if mode()? {
        verifier_from_env()?;
        if signing {
            issuer_from_env()?;
        }
    } else if std::env::var_os("KABIPAY_ORGANIZATION_POLICY_URL").is_some() {
        return Err(KabiPayError::Internal(
            "organization company runtimes require rs256 client JWT verification".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    const PRIVATE: &[u8] =
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-private.pem");
    const PUBLIC: &[u8] =
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-public.pem");
    fn claims() -> ClientClaims {
        serde_json::from_value(serde_json::json!({"sub":Uuid::from_u128(2),"tenant_id":Uuid::from_u128(1),"iss":CLIENT_JWT_ISSUER,"iat":chrono::Utc::now().timestamp(),"exp":chrono::Utc::now().timestamp()+60})).unwrap()
    }
    fn verifier() -> ClientJwtVerifier {
        ClientJwtVerifier::new(vec![ClientVerificationKey {
            kid: "company".into(),
            tenant_ids: vec![Uuid::from_u128(1)],
            public_pem: PUBLIC.to_vec(),
        }])
        .unwrap()
    }
    #[test]
    fn rsa_client_token_is_company_bound_and_only_auth_can_issue_it() {
        let issuer = ClientJwtIssuer::new("company", Uuid::from_u128(1), PRIVATE).unwrap();
        let c = claims();
        let token = issuer.issue(&c).unwrap();
        assert_eq!(verifier().verify(&token).unwrap().sub, c.sub);
        let mut cross = c;
        cross.tenant_id = Uuid::from_u128(3);
        assert!(issuer.issue(&cross).is_err());
        let cross_token = ClientJwtIssuer::new("company", cross.tenant_id, PRIVATE)
            .unwrap()
            .issue(&cross)
            .unwrap();
        assert!(verifier().verify(&cross_token).is_err());
    }
    #[test]
    fn rsa_verifier_rejects_hmac_operator_and_missing_type_tokens() {
        let c = claims();
        let hmac = crate::jwt::encode_client_jwt(&c, PUBLIC).unwrap();
        assert!(verifier().verify(&hmac).is_err());
        let mut h = Header::new(Algorithm::RS256);
        h.kid = Some("company".into());
        h.typ = Some("JWT".into());
        let token = jsonwebtoken::encode(
            &h,
            &Payload {
                aud: AUDIENCE.into(),
                nbf: c.iat,
                claims: c,
            },
            &EncodingKey::from_rsa_pem(PRIVATE).unwrap(),
        )
        .unwrap();
        assert!(verifier().verify(&token).is_err());
    }
    #[test]
    fn public_key_config_rejects_private_keys_and_unknown_company() {
        assert!(ClientJwtVerifier::new(vec![ClientVerificationKey {
            kid: "company".into(),
            tenant_ids: vec![Uuid::from_u128(1)],
            public_pem: PRIVATE.to_vec()
        }])
        .is_err());
        assert!(ClientJwtVerifier::new(vec![ClientVerificationKey {
            kid: "company".into(),
            tenant_ids: vec![],
            public_pem: PUBLIC.to_vec()
        }])
        .is_err());
    }
    #[test]
    fn a_client_key_cannot_be_registered_for_two_companies(){
      let a=Uuid::from_u128(1);let b=Uuid::from_u128(2);
      let key=|kid:&str,ids:Vec<Uuid>|ClientVerificationKey{kid:kid.into(),tenant_ids:ids,public_pem:PUBLIC.to_vec()};
      assert!(ClientJwtVerifier::new(vec![key("a",vec![a,b])]).is_err());
      assert!(ClientJwtVerifier::new(vec![key("a",vec![a]),key("b",vec![b])]).is_err());
    }

}
