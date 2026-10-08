//! Offline section readiness uses the same validators as the domain writers.
use crate::contract::{ImportEmployee, ImportPackage, SourceRef};
use kabipay_leave::services::leave_import_history::{validate_opening, OpeningSnapshot};
use kabipay_payroll::services::{
    payroll_rules::calculate_period,
    salary_rules::{validate_recurring_salary, RecurringSalary},
};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct RowReadiness {
    pub source_ref: SourceRef,
    pub core_ready: bool,
    pub salary_ready: bool,
    pub leave_ready: bool,
    pub period_ready: bool,
    pub period_code: String,
}

pub fn core_ready(package: &ImportPackage, row: &ImportEmployee) -> bool {
    row.core_ready().is_ok()
        && !package.issues.iter().any(|issue| {
            matches!(issue.severity.as_str(), "BLOCK_EMPLOYEE" | "BLOCK_TENANT")
                && (issue.source_ref.is_none()
                    || issue.source_ref.as_ref().is_some_and(|source| {
                        source.sheet == row.source_ref.sheet && source.row == row.source_ref.row
                    }))
        })
}

pub fn validate(package: &ImportPackage) -> Vec<RowReadiness> {
    package
        .employees
        .iter()
        .map(|row| {
            let salary_ready = row.recurring_salary.as_ref().is_some_and(|value| {
                serde_json::from_value::<RecurringSalary>(value.clone())
                    .ok()
                    .is_some_and(|salary| validate_recurring_salary(&salary).is_ok())
            }) && package.salary_start(row).is_ok();
            let leave_ready = row.leave_opening.as_ref().is_some_and(|value| {
                serde_json::from_value::<OpeningSnapshot>(value.clone())
                    .ok()
                    .is_some_and(|opening| {
                        opening.as_of == package.leave_as_of && validate_opening(&opening).is_ok()
                    })
            });
            let (period_ready, period_code) = match &row.period_input {
                None => (false, "PERIOD_NOT_SUPPLIED".into()),
                Some(period) if period.automatic.is_some() => (
                    false,
                    "AUTOMATIC_PERIOD_REQUIRES_TENANT_CONFIGURATION".into(),
                ),
                Some(period) => match calculate_period(period) {
                    Ok(_) if period.ready => (true, "PERIOD_FINANCIALLY_RECONCILED".into()),
                    Ok(_) => (false, "PERIOD_REVIEW_REQUIRED".into()),
                    Err(error) => (false, crate::cli::safe_error(&anyhow::Error::new(error))),
                },
            };
            RowReadiness {
                source_ref: row.source_ref.clone(),
                core_ready: core_ready(package, row),
                salary_ready,
                leave_ready,
                period_ready,
                period_code,
            }
        })
        .collect()
}
