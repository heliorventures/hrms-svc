//! Optional tax sections use the same HR domain writers and caller savepoints.
use anyhow::{bail, Result};
use kabipay_tax::domain::{TaxHistoryEntry, TaxSettingsInput};
use sea_orm::ConnectionTrait;
use serde_json::Value;
use uuid::Uuid;
pub fn settings_valid(value: Option<&Value>) -> Result<TaxSettingsInput> {
    let value = value.ok_or_else(|| anyhow::anyhow!("TAX_SETTINGS_NOT_SUPPLIED"))?;
    let input: TaxSettingsInput = serde_json::from_value(value.clone())
        .map_err(|_| anyhow::anyhow!("TAX_SETTINGS_REVIEW_REQUIRED"))?;
    input
        .validate()
        .map_err(|_| anyhow::anyhow!("TAX_SETTINGS_REVIEW_REQUIRED"))?;
    Ok(input)
}
pub fn history_valid(values: &[Value]) -> Result<Vec<TaxHistoryEntry>> {
    if values.len() > 120 {
        bail!("TAX_HISTORY_TOO_LARGE");
    }
    values
        .iter()
        .map(|v| {
            let entry: TaxHistoryEntry = serde_json::from_value(v.clone())
                .map_err(|_| anyhow::anyhow!("TAX_HISTORY_REVIEW_REQUIRED"))?;
            entry
                .validate()
                .map_err(|_| anyhow::anyhow!("TAX_HISTORY_REVIEW_REQUIRED"))?;
            Ok(entry)
        })
        .collect()
}
pub async fn settings<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    value: Option<&Value>,
) -> Result<(&'static str, &'static str)> {
    if value.is_none() {
        return Ok(("DEFERRED", "TAX_SETTINGS_NOT_SUPPLIED"));
    }
    let input = settings_valid(value)?;
    let current =
        kabipay_tax::services::tax_settings::list_tax_settings(db, tenant, employee).await?;
    if current.first().is_some_and(|r| r.input == input) {
        return Ok(("UNCHANGED", "TAX_SETTINGS_IMPORTED"));
    }
    // Import may establish an absent setting, not silently supersede HR's choice.
    if !current.is_empty() {
        bail!("TAX_SETTINGS_EXISTING_REVIEW_REQUIRED");
    }
    kabipay_tax::services::tax_settings::save_tax_settings(
        db, tenant, actor, employee, input, None,
    )
    .await
    .map_err(|_| anyhow::anyhow!("TAX_SETTINGS_IMPORT_FAILED"))?;
    Ok(("CREATED", "TAX_SETTINGS_IMPORTED"))
}
pub async fn history<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    values: &[Value],
) -> Result<(&'static str, &'static str)> {
    if values.is_empty() {
        return Ok(("DEFERRED", "TAX_HISTORY_NOT_SUPPLIED"));
    }
    let entries = history_valid(values)?;
    let mut changed = false;
    for entry in entries {
        let existing = kabipay_tax::services::tax_history::list_tax_history(
            db,
            tenant,
            employee,
            entry.fiscal_year,
        )
        .await?;
        let previous = existing
            .iter()
            .find(|r| r.entry.source_key == entry.source_key);
        if previous.is_some_and(|r| r.entry != entry) {
            bail!("TAX_HISTORY_EXISTING_REVIEW_REQUIRED");
        }
        changed |= previous.is_none();
        kabipay_tax::services::tax_history::save_tax_history(
            db,
            tenant,
            actor,
            employee,
            entry,
            previous.map(|r| r.revision),
        )
        .await
        .map_err(|_| anyhow::anyhow!("TAX_HISTORY_COVERAGE_OR_IMPORT_FAILED"))?;
    }
    Ok((
        if changed { "CREATED" } else { "UNCHANGED" },
        "TAX_HISTORY_IMPORTED",
    ))
}
