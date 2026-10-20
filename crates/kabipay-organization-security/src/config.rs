//! Explicit key files; errors never echo keys or environment values.
use crate::*;
use kabipay_organization_domain::TenantId;
use serde::Deserialize;
use std::{collections::HashMap, path::Path};
use uuid::Uuid;
fn env(name: &str) -> Result<String, DelegationError> {
    std::env::var(name).map_err(|_| DelegationError::Configuration)
}
fn pem(path: &str) -> Result<Vec<u8>, DelegationError> {
    let p = Path::new(path);
    if !p.is_absolute() {
        return Err(DelegationError::Configuration);
    }
    let meta = std::fs::metadata(p).map_err(|_| DelegationError::Configuration)?;
    if !meta.is_file() || meta.len() > 16384 {
        return Err(DelegationError::Configuration);
    }
    std::fs::read(p).map_err(|_| DelegationError::Configuration)
}
pub fn service_issuer_from_env() -> Result<ServiceCredentialIssuer, DelegationError> {
    ServiceCredentialIssuer::new(
        &env("KABIPAY_SERVICE_ID")?,
        &env("KABIPAY_SERVICE_KEY_ID")?,
        &pem(&env("KABIPAY_SERVICE_PRIVATE_KEY_FILE")?)?,
    )
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TrustedConfig {
    kid: String,
    service: String,
    tenant_ids: Vec<Uuid>,
    public_key_file: String,
}
pub fn service_verifier_from_env(
    audience: &str,
) -> Result<ServiceCredentialVerifier, DelegationError> {
    let input = env("KABIPAY_SERVICE_TRUST_KEYS")?;
    if input.len() > 65536 {
        return Err(DelegationError::Configuration);
    }
    let configs: Vec<TrustedConfig> =
        serde_json::from_str(&input).map_err(|_| DelegationError::Configuration)?;
    let keys = configs
        .into_iter()
        .map(|c| {
            Ok(ServiceVerificationKey {
                kid: c.kid,
                service: c.service,
                tenant_ids: c.tenant_ids,
                public_pem: pem(&c.public_key_file)?,
            })
        })
        .collect::<Result<Vec<_>, DelegationError>>()?;
    ServiceCredentialVerifier::new(audience, keys)
}
pub fn delegation_verifier_from_env(
    tenant: Uuid,
    audience: &str,
) -> Result<DelegationVerifier, DelegationError> {
    let input = env("KABIPAY_ORGANIZATION_DELEGATION_PUBLIC_KEYS")?;
    if input.len() > 65536 {
        return Err(DelegationError::Configuration);
    }
    let paths: HashMap<String, String> =
        serde_json::from_str(&input).map_err(|_| DelegationError::Configuration)?;
    let keys = paths
        .into_iter()
        .map(|(k, p)| Ok((k, pem(&p)?)))
        .collect::<Result<Vec<_>, DelegationError>>()?;
    DelegationVerifier::new(
        TenantId::try_from(tenant).map_err(|_| DelegationError::Configuration)?,
        audience,
        keys.iter()
            .map(|(k, p)| (k.as_str(), p.as_slice()))
            .collect(),
    )
}
pub fn delegation_issuer_from_env() -> Result<DelegationIssuer, DelegationError> {
    DelegationIssuer::new(
        &env("KABIPAY_ORGANIZATION_DELEGATION_KEY_ID")?,
        &pem(&env("KABIPAY_ORGANIZATION_DELEGATION_PRIVATE_KEY_FILE")?)?,
    )
}
