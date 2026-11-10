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
    #[serde(default)]
    shared_runtime: bool,
}
pub fn service_verifier_from_env(
    audience: &str,
) -> Result<ServiceCredentialVerifier, DelegationError> {
    let input = env("KABIPAY_SERVICE_TRUST_KEYS")?;
    service_verifier_from_config(audience, &input)
}
fn service_verifier_from_config(
    audience: &str,
    input: &str,
) -> Result<ServiceCredentialVerifier, DelegationError> {
    if input.len() > 65536 {
        return Err(DelegationError::Configuration);
    }
    let configs: Vec<TrustedConfig> =
        serde_json::from_str(input).map_err(|_| DelegationError::Configuration)?;
    let shared: Vec<_> = configs
        .iter()
        .filter(|c| c.shared_runtime)
        .map(|c| c.kid.clone())
        .collect();
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
    let shared_refs: Vec<_> = shared.iter().map(String::as_str).collect();
    ServiceCredentialVerifier::new_with_shared_keys(audience, keys, &shared_refs)
}

#[cfg(test)]
mod shared_trust_tests {
    use super::{service_verifier_from_config, TrustedConfig};
    use crate::ServiceCredentialIssuer;
    use uuid::Uuid;

    #[test]
    fn trusted_configuration_accepts_explicit_shared_runtime_opt_in() {
        let input = r#"{"kid":"shared-leave-v1","service":"kabipay-leave","tenant_ids":["11111111-1111-4111-8111-111111111111","22222222-2222-4222-8222-222222222222"],"public_key_file":"/run/public-keys/leave.pem","shared_runtime":true}"#;
        assert!(
            serde_json::from_str::<TrustedConfig>(input)
                .unwrap()
                .shared_runtime
        );
    }

    #[test]
    fn deployment_configuration_requires_opt_in_and_checks_actual_signed_companies() {
        let public = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("tests/fixtures/test-a-public.pem");
        let a = Uuid::from_u128(1);
        let b = Uuid::from_u128(2);
        let mut entry = serde_json::json!({"kid":"shared-leave-v1","service":"kabipay-leave","tenant_ids":[a,b],"public_key_file":public});
        let config = |entry: &serde_json::Value| serde_json::to_string(&vec![entry]).unwrap();
        assert!(service_verifier_from_config("kabipay-organization", &config(&entry)).is_err());
        entry["shared_runtime"] = false.into();
        assert!(service_verifier_from_config("kabipay-organization", &config(&entry)).is_err());
        entry["shared_runtime"] = true.into();
        let verifier =
            service_verifier_from_config("kabipay-organization", &config(&entry)).unwrap();
        let issuer = ServiceCredentialIssuer::new(
            "kabipay-leave",
            "shared-leave-v1",
            include_bytes!("../tests/fixtures/test-a-private.pem"),
        )
        .unwrap();
        for tenant in [a, b] {
            let proof = issuer
                .issue(
                    "kabipay-organization",
                    tenant,
                    "/internal/organization/policy",
                    b"request",
                )
                .unwrap();
            assert!(verifier
                .verify(&proof, tenant, "/internal/organization/policy", b"request")
                .is_ok());
        }
        let unlisted = Uuid::from_u128(3);
        let proof = issuer
            .issue(
                "kabipay-organization",
                unlisted,
                "/internal/organization/policy",
                b"request",
            )
            .unwrap();
        assert!(verifier
            .verify(
                &proof,
                unlisted,
                "/internal/organization/policy",
                b"request"
            )
            .is_err());
        entry["shared_runtime"] = "true".into();
        assert!(service_verifier_from_config("kabipay-organization", &config(&entry)).is_err());
    }
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
