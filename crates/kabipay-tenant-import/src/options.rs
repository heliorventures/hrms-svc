//! Explicit operator target, retained identities and optional employee login manifest.
use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportOptions {
    pub tenant_id: Uuid,
    pub tenant_code: String,
    pub schema_name: String,
    pub db_host: String,
    #[serde(default)]
    pub connection_host: Option<String>,
    pub db_name: String,
    pub actor_id: Uuid,
    pub preserved_usernames: Vec<String>,
    pub reviewed_absent_usernames: Vec<String>,
    pub login_by_employee_code: BTreeMap<String, String>,
    pub payroll_excluded_employee_ids: Vec<Uuid>,
    pub runtime_contract_version: u32,
    #[serde(default)]
    pub review_reference: Option<String>,
    #[serde(default)]
    pub reset_delete_tables: Vec<String>,
    #[serde(default)]
    pub reset_retain_tables: Vec<String>,
    #[serde(default)]
    pub reset_truncate_tables: Vec<String>,
    #[serde(default)]
    pub replacement_backup: crate::backup::BackupPolicy,
}
impl ImportOptions {
    pub fn validate(&self) -> Result<()> {
        self.replacement_backup.validate()?;
        if !valid_schema(&self.schema_name)
            || self.runtime_contract_version != 1
            || self.tenant_code.trim().is_empty()
            || self.db_host.trim().is_empty()
            || self.db_name.trim().is_empty()
        {
            bail!("TARGET_CONFIGURATION_INVALID");
        }
        if self
            .connection_host
            .as_ref()
            .is_some_and(|value| value.trim().is_empty())
            || self.review_reference.as_ref().is_some_and(|value| {
                value.trim().is_empty() || value.len() > 200 || value.chars().any(char::is_control)
            })
        {
            bail!("TARGET_CONFIGURATION_INVALID");
        }
        let mut names = HashSet::new();
        for value in self
            .preserved_usernames
            .iter()
            .chain(self.reviewed_absent_usernames.iter())
        {
            if value.trim().is_empty() || !names.insert(value.to_lowercase()) {
                bail!("PRESERVATION_MANIFEST_INVALID");
            }
        }
        let mut logins = HashSet::new();
        for value in self.login_by_employee_code.values() {
            if value.len() > 128
                || value.trim() != value
                || value.is_empty()
                || !logins.insert(value.to_lowercase())
                || names.contains(&value.to_lowercase())
            {
                bail!("LOGIN_MANIFEST_INVALID");
            }
        }
        Ok(())
    }
}
pub fn valid_schema(value: &str) -> bool {
    value.starts_with("tenant_")
        && value.len() <= 63
        && value.len() > 7
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}
