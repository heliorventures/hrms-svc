//! Operational selectors only. Unknown/duplicate flags never echo their values.
use super::rotation::{RotationError, RotationMode, RotationRequest};
use std::{collections::BTreeMap, path::PathBuf};
use uuid::Uuid;

pub const ROTATION_USAGE: &str = "kabipay-email-key-rotation --tenant UUID --expected-database company_NAME --schema NAME --expected-version N --current-key-id ID --new-key-id ID --database-url-file ABSOLUTE_FILE --keyring-file ABSOLUTE_FILE [--apply]\nDefaults to preview. Never pass credentials or key bytes as arguments. Keep both keys until all companies and recovery copies are verified.";
#[derive(Debug)]
pub struct RotationOptions {
    pub request: RotationRequest,
    pub database_url_file: PathBuf,
    pub keyring_file: PathBuf,
}
pub fn parse_options(arguments: &[String]) -> Result<RotationOptions, RotationError> {
    let mut values = BTreeMap::new();
    let mut apply = false;
    let mut index = 0;
    while index < arguments.len() {
        let key = &arguments[index];
        if key == "--apply" {
            if apply {
                return Err(RotationError::Configuration);
            }
            apply = true;
            index += 1;
            continue;
        }
        if ![
            "--tenant",
            "--expected-database",
            "--schema",
            "--expected-version",
            "--current-key-id",
            "--new-key-id",
            "--database-url-file",
            "--keyring-file",
        ]
        .contains(&key.as_str())
        {
            return Err(RotationError::Configuration);
        }
        let value = arguments
            .get(index + 1)
            .filter(|value| !value.starts_with("--"))
            .ok_or(RotationError::Configuration)?;
        if values.insert(key.as_str(), value.as_str()).is_some() {
            return Err(RotationError::Configuration);
        }
        index += 2;
    }
    let get = |key| values.get(key).copied().ok_or(RotationError::Configuration);
    let request = RotationRequest {
        tenant_id: Uuid::parse_str(get("--tenant")?).map_err(|_| RotationError::Configuration)?,
        expected_database: get("--expected-database")?.into(),
        expected_schema: get("--schema")?.into(),
        expected_version: get("--expected-version")?
            .parse()
            .map_err(|_| RotationError::Configuration)?,
        current_key_id: get("--current-key-id")?.into(),
        new_key_id: get("--new-key-id")?.into(),
        mode: if apply {
            RotationMode::Apply
        } else {
            RotationMode::Preview
        },
    };
    request.validate()?;
    let database_url_file = PathBuf::from(get("--database-url-file")?);
    let keyring_file = PathBuf::from(get("--keyring-file")?);
    if !database_url_file.is_absolute()
        || !keyring_file.is_absolute()
        || database_url_file == keyring_file
    {
        return Err(RotationError::Configuration);
    }
    Ok(RotationOptions {
        request,
        database_url_file,
        keyring_file,
    })
}
