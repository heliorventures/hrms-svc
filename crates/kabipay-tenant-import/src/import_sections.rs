//! Independent domain section writers operate inside the caller savepoint.
use crate::contract::{ImportEmployee, ImportPackage};
use anyhow::{bail, Result};
use sea_orm::DatabaseTransaction;
use uuid::Uuid;
pub async fn write_section(
    transaction: &DatabaseTransaction,
    package: &ImportPackage,
    row: &ImportEmployee,
    options: &crate::options::ImportOptions,
    employee: Uuid,
    section: &str,
) -> Result<(&'static str, &'static str)> {
    let tenant = options.tenant_id;
    let actor = options.actor_id;
    match section {
        "tax_settings" => {
            crate::tax_import::settings(
                transaction,
                tenant,
                actor,
                employee,
                row.tax_settings.as_ref(),
            )
            .await
        }
        "tax_history" => {
            if let Some(period) = &row.period_input {
                let (start, end) =
                    kabipay_tax::domain::tax_year::month_bounds(period.year, period.month as u32)?;
                if crate::tax_import::history_valid(&row.tax_history)?
                    .iter()
                    .any(|entry| {
                        entry.employer == "CURRENT"
                            && entry.period_start <= end
                            && entry.period_end >= start
                    })
                {
                    bail!("TAX_HISTORY_OVERLAPS_PACKAGE_PAYROLL");
                }
            }
            crate::tax_import::history(transaction, tenant, actor, employee, &row.tax_history).await
        }
        "department" => Ok((
            crate::organization_import::department(transaction, tenant, employee, row).await?,
            "DEPARTMENT_IMPORTED",
        )),
        "designation" => Ok((
            crate::organization_import::designation(transaction, tenant, employee, row).await?,
            "DESIGNATION_IMPORTED",
        )),
        "profile" => Ok((
            crate::employee_import::optional_profile(transaction, tenant, employee, row).await?,
            "PROFILE_IMPORTED",
        )),
        "identity" => {
            if row.identity.is_none()
                && !row
                    .clear_fields
                    .iter()
                    .any(|field| matches!(field.as_str(), "pan" | "aadhaar_last_four"))
            {
                return Ok(("DEFERRED", "IDENTITY_NOT_SUPPLIED"));
            }
            Ok((
                crate::employee_import::identity(transaction, tenant, employee, row).await?,
                "IDENTITY_IMPORTED",
            ))
        }
        "bank" => {
            if row.bank.is_none()
                && !row
                    .clear_fields
                    .iter()
                    .any(|field| field.starts_with("bank."))
            {
                return Ok(("DEFERRED", "BANK_NOT_SUPPLIED"));
            }
            Ok((
                crate::employee_import::bank(transaction, tenant, employee, row).await?,
                "BANK_IMPORTED",
            ))
        }
        "recurring_salary" => {
            let Some(salary) = &row.recurring_salary else {
                return Ok(("DEFERRED", "SALARY_NOT_READY"));
            };
            Ok((
                crate::salary_import::salary(
                    transaction,
                    tenant,
                    actor,
                    employee,
                    package.salary_start(row)?,
                    salary,
                )
                .await?,
                "SALARY_IMPORTED",
            ))
        }
        "leave_opening" => {
            let Some(value) = &row.leave_opening else {
                return Ok(("DEFERRED", "LEAVE_NOT_SUPPLIED"));
            };
            let snapshot: kabipay_leave::services::leave_import_history::OpeningSnapshot =
                serde_json::from_value(value.clone())?;
            if snapshot.as_of != package.leave_as_of {
                bail!("LEAVE_DATE_DISAGREES");
            }
            let paid = package.configuration["leave"]["paid_type_code"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("LEAVE_CONFIGURATION_REQUIRED"))?;
            let unpaid = package.configuration["leave"]["unpaid_type_code"]
                .as_str()
                .ok_or_else(|| anyhow::anyhow!("LEAVE_CONFIGURATION_REQUIRED"))?;
            let paid_type = kabipay_leave::services::leave_import_history::ensure_type(
                transaction,
                tenant,
                paid,
                true,
            )
            .await?;
            kabipay_leave::services::leave_import_history::ensure_type(
                transaction,
                tenant,
                unpaid,
                false,
            )
            .await?;
            let historical = if let Some(history) = &row.historical_lwp {
                if history["payroll_attribution"] != "HISTORICAL_ONLY"
                    || history["as_of"] != package.leave_as_of.to_string()
                {
                    bail!("HISTORICAL_LEAVE_ATTRIBUTION_INVALID");
                }
                kabipay_payroll::services::payroll_rules::amount(
                    history["days"].as_str(),
                    "historical_lwp",
                )?
            } else if snapshot.ready {
                kabipay_leave::services::leave_import_history::validate_opening(&snapshot)?
                    .historical_lwp
            } else {
                rust_decimal::Decimal::ZERO
            };
            let ready = snapshot.ready;
            let outcome = kabipay_leave::services::leave_import_history::save(
                transaction,
                tenant,
                actor,
                employee,
                paid_type,
                snapshot,
                historical,
                serde_json::to_value(&row.source_ref)?,
            )
            .await?;
            Ok((
                if ready { outcome } else { "DEFERRED" },
                if ready {
                    "LEAVE_IMPORTED"
                } else {
                    "LEAVE_STAGED_REQUIRES_REVIEW"
                },
            ))
        }
        "period_input" => {
            let Some(input) = &row.period_input else {
                return Ok(("DEFERRED", "PERIOD_NOT_SUPPLIED"));
            };
            let existing = kabipay_payroll::services::payroll_period_input::find(
                transaction,
                tenant,
                employee,
                input.year,
                input.month,
            )
            .await?;
            let prior_revision = existing.as_ref().map(|row| row.revision);
            let saved = kabipay_payroll::services::payroll_period_input::save(
                transaction,
                tenant,
                actor,
                employee,
                input.clone(),
                Some(serde_json::to_value(&row.source_ref)?),
                existing.map(|row| row.revision),
            )
            .await?;
            let outcome = if !saved.ready {
                "DEFERRED"
            } else if prior_revision == Some(saved.revision) {
                "UNCHANGED"
            } else if prior_revision.is_none() {
                "CREATED"
            } else {
                "UPDATED"
            };
            Ok((
                outcome,
                if saved.ready {
                    "PERIOD_READY"
                } else {
                    "PERIOD_STAGED_REQUIRES_REVIEW"
                },
            ))
        }
        _ => bail!("SECTION_UNSUPPORTED"),
    }
}
