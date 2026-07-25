//! Deterministic tenant UUIDs; must match `kabipay-database/scripts/provision-tenant.ps1` (`New-DeterministicUuid`).

use sha1::{Digest, Sha1};
use uuid::Uuid;

fn uuid_from_kabipay_seed(seed: &str) -> Uuid {
    let mut hasher = Sha1::new();
    hasher.update(seed.as_bytes());
    let digest = hasher.finalize();
    let mut bytes: [u8; 16] = digest[..16].try_into().expect("sha1 truncated to 16 bytes");
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}

/// Tenant row id for a short `code` (e.g. `demo` → same id as seed script).
pub fn deterministic_tenant_uuid(code: &str) -> Uuid {
    uuid_from_kabipay_seed(&format!("kabipay-tenant:{code}"))
}

/// `tenant_database` row id for `code` (seed `kabipay-tenant:{code}-db`).
pub fn deterministic_tenant_database_row_uuid(code: &str) -> Uuid {
    uuid_from_kabipay_seed(&format!("kabipay-tenant:{code}-db"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn demo_code_matches_provision_script() {
        let id = deterministic_tenant_uuid("demo");
        assert_eq!(
            id.to_string(),
            "342205fc-98b1-5421-8a11-b30821c86aa0",
            "must stay aligned with kabipay-database/scripts/provision-tenant.ps1"
        );
    }
}
