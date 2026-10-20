use jsonwebtoken::{DecodingKey, EncodingKey};
use kabipay_organization_domain::{
    Capability, MembershipId, OrganizationId, PrincipalId, ResourceVersion, TenantId,
};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use uuid::Uuid;

pub const DELEGATION_ISSUER: &str = "kabipay-organization-delegation";
pub const DELEGATION_TOKEN_TYPE: &str = "hrms-delegation+jwt";
pub const MAX_DELEGATION_LIFETIME_SECS: u64 = 300;
#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum DelegationError {
    #[error("delegation configuration is invalid")]
    Configuration,
    #[error("delegation claims are invalid")]
    Claims,
    #[error("delegation token is invalid or expired")]
    Token,
    #[error("delegation does not match the requested command")]
    Command,
}
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum AuthorityKind {
    OrganizationGrant,
    ManagementAssignment,
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DelegationClaims {
    pub iss: String,
    pub aud: String,
    pub sub: PrincipalId,
    pub jti: Uuid,
    pub iat: u64,
    pub nbf: u64,
    pub exp: u64,
    pub organization_id: OrganizationId,
    pub target_membership_id: MembershipId,
    pub source_tenant_id: TenantId,
    pub source_user_id: Uuid,
    pub target_tenant_id: TenantId,
    pub identity_version: ResourceVersion,
    pub source_link_version: ResourceVersion,
    pub target_membership_version: ResourceVersion,
    pub authority_kind: AuthorityKind,
    pub authority_ref: Uuid,
    pub authority_version: ResourceVersion,
    pub capability: Capability,
    pub resource_type: String,
    pub resource_id: Uuid,
    pub expected_resource_version: ResourceVersion,
    pub command_id: Uuid,
    pub request_hash: String,
}
pub struct CommandExpectation<'a> {
    pub capability: &'a Capability,
    pub resource_type: &'a str,
    pub resource_id: Uuid,
    pub expected_resource_version: ResourceVersion,
    pub command_id: Uuid,
    pub request_hash: &'a str,
}
pub struct VerifiedDelegation {
    claims: DelegationClaims,
}
impl VerifiedDelegation {
    pub fn claims(&self) -> &DelegationClaims {
        &self.claims
    }
}
pub struct DelegationIssuer {
    kid: String,
    key: EncodingKey,
}
pub struct DelegationVerifier {
    target: TenantId,
    audience: String,
    keys: HashMap<String, DecodingKey>,
}
pub fn command_body_hash(body: &[u8]) -> String {
    use sha2::Digest;
    hex::encode(sha2::Sha256::digest(body))
}
impl DelegationIssuer {
    pub fn new(kid: &str, pem: &[u8]) -> Result<Self, DelegationError> {
        if !key_id(kid) {
            return Err(DelegationError::Configuration);
        }
        Ok(Self {
            kid: kid.into(),
            key: EncodingKey::from_rsa_pem(pem).map_err(|_| DelegationError::Configuration)?,
        })
    }
    /// Only the trusted issuer may call this after current source identity, authority
    /// and exact command scope have been authorized. This is not a policy check.
    pub fn issue(&self, claims: &DelegationClaims) -> Result<String, DelegationError> {
        claims.validate(now()?)?;
        let mut header = jsonwebtoken::Header::new(jsonwebtoken::Algorithm::RS256);
        header.kid = Some(self.kid.clone());
        header.typ = Some(DELEGATION_TOKEN_TYPE.into());
        jsonwebtoken::encode(&header, claims, &self.key).map_err(|_| DelegationError::Token)
    }
}
impl DelegationVerifier {
    pub fn new(
        target: TenantId,
        audience: &str,
        keys: Vec<(&str, &[u8])>,
    ) -> Result<Self, DelegationError> {
        if !service_name(audience) || keys.is_empty() {
            return Err(DelegationError::Configuration);
        }
        let mut parsed = HashMap::new();
        for (kid, pem) in keys {
            if !key_id(kid)
                || parsed.contains_key(kid)
                || pem.windows(11).any(|part| part == b"PRIVATE KEY")
            {
                return Err(DelegationError::Configuration);
            }
            parsed.insert(
                kid.into(),
                DecodingKey::from_rsa_pem(pem).map_err(|_| DelegationError::Configuration)?,
            );
        }
        Ok(Self {
            target,
            audience: audience.into(),
            keys: parsed,
        })
    }
    /// Authenticate this exact command. The result still needs live authorization,
    /// target-domain validation and a transactionally persisted command receipt.
    pub fn verify(
        &self,
        token: &str,
        expected: &CommandExpectation<'_>,
    ) -> Result<VerifiedDelegation, DelegationError> {
        if token.len() > 16384 {
            return Err(DelegationError::Token);
        }
        let header = jsonwebtoken::decode_header(token).map_err(|_| DelegationError::Token)?;
        if header.alg != jsonwebtoken::Algorithm::RS256
            || header.typ.as_deref() != Some(DELEGATION_TOKEN_TYPE)
        {
            return Err(DelegationError::Token);
        }
        let key = self
            .keys
            .get(header.kid.as_deref().unwrap_or_default())
            .ok_or(DelegationError::Token)?;
        let mut validation = jsonwebtoken::Validation::new(jsonwebtoken::Algorithm::RS256);
        validation.set_issuer(&[DELEGATION_ISSUER]);
        validation.set_audience(&[&self.audience]);
        validation.set_required_spec_claims(&["exp", "nbf", "iss", "aud", "sub"]);
        validation.leeway = 0;
        validation.validate_nbf = true;
        let claims = jsonwebtoken::decode::<DelegationClaims>(token, key, &validation)
            .map_err(|_| DelegationError::Token)?
            .claims;
        claims.validate(now()?)?;
        if claims.target_tenant_id != self.target
            || &claims.capability != expected.capability
            || claims.resource_type != expected.resource_type
            || claims.resource_id != expected.resource_id
            || claims.expected_resource_version != expected.expected_resource_version
            || claims.command_id != expected.command_id
            || claims.request_hash != expected.request_hash
        {
            return Err(DelegationError::Command);
        }
        Ok(VerifiedDelegation { claims })
    }
}
impl DelegationClaims {
    fn validate(&self, now: u64) -> Result<(), DelegationError> {
        let lifetime = self
            .exp
            .checked_sub(self.iat)
            .ok_or(DelegationError::Claims)?;
        if self.iss != DELEGATION_ISSUER
            || !service_name(&self.aud)
            || self.iat > now
            || self.nbf != self.iat
            || self.exp <= now
            || lifetime == 0
            || lifetime > MAX_DELEGATION_LIFETIME_SECS
            || !service_name(&self.resource_type)
            || [
                self.jti,
                self.source_user_id,
                self.authority_ref,
                self.resource_id,
                self.command_id,
            ]
            .iter()
            .any(Uuid::is_nil)
            || self.request_hash.len() != 64
            || !self
                .request_hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(DelegationError::Claims);
        }
        Ok(())
    }
}
pub(crate) fn now() -> Result<u64, DelegationError> {
    u64::try_from(chrono::Utc::now().timestamp()).map_err(|_| DelegationError::Claims)
}
pub(crate) fn service_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        })
}
pub(crate) fn key_id(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
