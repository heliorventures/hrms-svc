//! A company credential proves one configured service/company, never an end user.
use crate::{
    command_body_hash,
    delegation::{key_id, now, service_name},
    DelegationError,
};
use jsonwebtoken::{Algorithm, DecodingKey, EncodingKey, Header, Validation};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;
pub const SERVICE_ISSUER: &str = "kabipay-internal-service";
pub const SERVICE_TOKEN_TYPE: &str = "hrms-service+jwt";
const MAX_LIFETIME: u64 = 60;
const MAX_SHARED_COMPANIES: usize = 100;
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceCredentialClaims {
    pub iss: String,
    pub aud: String,
    pub service: String,
    pub tenant_id: Uuid,
    pub jti: Uuid,
    pub iat: u64,
    pub nbf: u64,
    pub exp: u64,
    pub method: String,
    pub route: String,
    pub request_hash: String,
}
pub struct ServiceVerificationKey {
    pub kid: String,
    pub service: String,
    pub tenant_ids: Vec<Uuid>,
    pub public_pem: Vec<u8>,
}
pub struct ServiceCredentialIssuer {
    service: String,
    kid: String,
    key: EncodingKey,
}
pub struct ServiceCredentialVerifier {
    audience: String,
    keys: HashMap<String, TrustedKey>,
}
struct TrustedKey {
    service: String,
    tenants: HashSet<Uuid>,
    key: DecodingKey,
}
pub struct VerifiedServiceCredential {
    claims: ServiceCredentialClaims,
}
impl VerifiedServiceCredential {
    pub fn claims(&self) -> &ServiceCredentialClaims {
        &self.claims
    }
}
fn valid_route(route: &str) -> bool {
    route.starts_with('/')
        && route.len() <= 160
        && route
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'/' | b'-' | b'_'))
        && !route.contains("//")
}
impl ServiceCredentialIssuer {
    pub fn new(service: &str, kid: &str, pem: &[u8]) -> Result<Self, DelegationError> {
        if !service_name(service) || !key_id(kid) {
            return Err(DelegationError::Configuration);
        }
        Ok(Self {
            service: service.into(),
            kid: kid.into(),
            key: EncodingKey::from_rsa_pem(pem).map_err(|_| DelegationError::Configuration)?,
        })
    }
    pub fn issue(
        &self,
        audience: &str,
        tenant: Uuid,
        route: &str,
        body: &[u8],
    ) -> Result<String, DelegationError> {
        if !service_name(audience) || tenant.is_nil() || !valid_route(route) || body.len() > 131072
        {
            return Err(DelegationError::Claims);
        }
        let now = now()?;
        let claims = ServiceCredentialClaims {
            iss: SERVICE_ISSUER.into(),
            aud: audience.into(),
            service: self.service.clone(),
            tenant_id: tenant,
            jti: Uuid::new_v4(),
            iat: now,
            nbf: now,
            exp: now + MAX_LIFETIME,
            method: "POST".into(),
            route: route.into(),
            request_hash: command_body_hash(body),
        };
        let mut header = Header::new(Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        header.typ = Some(SERVICE_TOKEN_TYPE.into());
        jsonwebtoken::encode(&header, &claims, &self.key).map_err(|_| DelegationError::Token)
    }
}
impl ServiceCredentialVerifier {
    pub fn new(audience: &str, keys: Vec<ServiceVerificationKey>) -> Result<Self, DelegationError> {
        Self::new_with_shared_keys(audience, keys, &[])
    }

    /// Only operator-reviewed key IDs may use an explicit multi-company scope.
    /// The default constructor retains the isolated-company restriction.
    pub fn new_with_shared_keys(
        audience: &str,
        keys: Vec<ServiceVerificationKey>,
        shared_kids: &[&str],
    ) -> Result<Self, DelegationError> {
        let shared: HashSet<_> = shared_kids.iter().copied().collect();
        if !service_name(audience)
            || keys.is_empty()
            || shared.len() != shared_kids.len()
            || shared.iter().any(|kid| !key_id(kid))
        {
            return Err(DelegationError::Configuration);
        }
        let mut parsed = HashMap::new();
        let mut identities = HashMap::new();
        for entry in keys {
            let tenants: HashSet<_> = entry.tenant_ids.iter().copied().collect();
            if !key_id(&entry.kid)
                || !service_name(&entry.service)
                || tenants.is_empty()
                || (entry.service != "kabipay-organization"
                    && tenants.len() != 1
                    && !shared.contains(entry.kid.as_str()))
                || (shared.contains(entry.kid.as_str()) && tenants.len() > MAX_SHARED_COMPANIES)
                || tenants.contains(&Uuid::nil())
                || tenants.len() != entry.tenant_ids.len()
                || parsed.contains_key(&entry.kid)
                || entry.public_pem.windows(11).any(|p| p == b"PRIVATE KEY")
            {
                return Err(DelegationError::Configuration);
            }
            let material: Vec<u8> = entry
                .public_pem
                .iter()
                .copied()
                .filter(|b| !b.is_ascii_whitespace())
                .collect();
            let identity = (entry.service.clone(), tenants.clone());
            if identities
                .get(&material)
                .is_some_and(|existing| existing != &identity)
            {
                return Err(DelegationError::Configuration);
            }
            identities.insert(material, identity);
            parsed.insert(
                entry.kid,
                TrustedKey {
                    service: entry.service,
                    tenants,
                    key: DecodingKey::from_rsa_pem(&entry.public_pem)
                        .map_err(|_| DelegationError::Configuration)?,
                },
            );
        }
        if shared.iter().any(|kid| !parsed.contains_key(*kid)) {
            return Err(DelegationError::Configuration);
        }
        Ok(Self {
            audience: audience.into(),
            keys: parsed,
        })
    }
    pub fn verify(
        &self,
        token: &str,
        tenant: Uuid,
        route: &str,
        body: &[u8],
    ) -> Result<VerifiedServiceCredential, DelegationError> {
        if tenant.is_nil() || !valid_route(route) || token.len() > 16384 || body.len() > 131072 {
            return Err(DelegationError::Token);
        }
        let h = jsonwebtoken::decode_header(token).map_err(|_| DelegationError::Token)?;
        if h.alg != Algorithm::RS256 || h.typ.as_deref() != Some(SERVICE_TOKEN_TYPE) {
            return Err(DelegationError::Token);
        }
        let trusted = self
            .keys
            .get(h.kid.as_deref().unwrap_or_default())
            .ok_or(DelegationError::Token)?;
        let mut validation = Validation::new(Algorithm::RS256);
        validation.set_issuer(&[SERVICE_ISSUER]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "nbf", "iss", "aud"]);
        validation.leeway = 0;
        validation.validate_nbf = true;
        let c = jsonwebtoken::decode::<ServiceCredentialClaims>(token, &trusted.key, &validation)
            .map_err(|_| DelegationError::Token)?
            .claims;
        let now = now()?;
        if c.service != trusted.service
            || c.tenant_id != tenant
            || !trusted.tenants.contains(&tenant)
            || c.method != "POST"
            || c.route != route
            || c.request_hash != command_body_hash(body)
            || c.jti.is_nil()
            || c.iat > now
            || c.nbf != c.iat
            || c.exp <= now
            || c.exp
                .checked_sub(c.iat)
                .is_none_or(|n| n == 0 || n > MAX_LIFETIME)
        {
            return Err(DelegationError::Command);
        }
        Ok(VerifiedServiceCredential { claims: c })
    }
}
