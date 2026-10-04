//! Versioned client-independent package. Financial source values remain decimal strings.
use anyhow::{bail, Result};
use chrono::NaiveDate;
use kabipay_payroll::services::payroll_rules::PeriodInput;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, HashSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportPackage {
    pub format: String,
    pub version: u32,
    pub source: Source,
    pub tenant_code: String,
    pub salary_effective_from: Option<NaiveDate>,
    #[serde(default = "fixed_date_policy")]
    pub salary_effective_policy: String,
    pub leave_as_of: NaiveDate,
    pub period: Period,
    pub employees: Vec<ImportEmployee>,
    pub configuration: Value,
    pub issues: Vec<ImportIssue>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Source {
    pub file_label: String,
    pub file_hash: String,
    pub profile: String,
    pub profile_version: u32,
    pub formula_results: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Period {
    pub year: i32,
    pub month: i32,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceRef {
    pub file_hash: String,
    pub sheet: String,
    pub row: u32,
    pub cells: BTreeMap<String, String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportIssue {
    pub code: String,
    pub severity: String,
    pub section: String,
    pub source_ref: Option<SourceRef>,
    pub field: String,
    pub message: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ImportEmployee {
    pub source_ref: SourceRef,
    pub source_states: BTreeMap<String, String>,
    pub employee: Employee,
    pub identity: Option<Identity>,
    pub bank: Option<Bank>,
    pub recurring_salary: Option<Value>,
    pub leave_opening: Option<Value>,
    pub historical_lwp: Option<Value>,
    pub period_input: Option<PeriodInput>,
    pub clear_fields: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Employee {
    pub code: Option<String>,
    pub mapping_key: String,
    pub name: Option<String>,
    pub first_name: Option<String>,
    pub last_name: Option<String>,
    pub joining_date: Option<NaiveDate>,
    pub birth_date: Option<NaiveDate>,
    pub confirmation_date: Option<NaiveDate>,
    pub exit_date: Option<NaiveDate>,
    pub last_working_date: Option<NaiveDate>,
    pub gender: Option<String>,
    pub designation: Option<String>,
    pub department: Option<String>,
    pub uan: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Identity {
    pub pan: Option<String>,
    pub aadhaar_last_four: Option<String>,
    pub verified: bool,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Bank {
    pub account_number: String,
    pub ifsc: String,
    pub bank_name: String,
    pub account_holder: Option<String>,
    pub branch: Option<String>,
    pub account_type: Option<String>,
    pub verified: bool,
}

impl ImportEmployee {
    pub fn core_ready(&self) -> Result<()> {
        let data = &self.employee;
        let Some(code) = data.code.as_deref() else {
            bail!("EMPLOYEE_CODE_REQUIRED")
        };
        if code.trim().is_empty()
            || code.len() > 64
            || code.chars().any(char::is_control)
            || data
                .first_name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
            || data
                .last_name
                .as_deref()
                .is_none_or(|name| name.trim().is_empty())
            || data.joining_date.is_none()
        {
            bail!("EMPLOYEE_IDENTITY_REQUIRED");
        }
        Ok(())
    }
}

impl ImportPackage {
    pub fn salary_start(&self, row: &ImportEmployee) -> Result<NaiveDate> {
        if self.salary_effective_policy == "JOINING_DATE" {
            row.employee
                .joining_date
                .ok_or_else(|| anyhow::anyhow!("SALARY_JOINING_DATE_REQUIRED"))
        } else {
            self.salary_effective_from
                .ok_or_else(|| anyhow::anyhow!("SALARY_EFFECTIVE_DATE_REQUIRED"))
        }
    }

    pub fn parse(bytes: &[u8]) -> Result<Self> {
        if bytes.len() > 20 * 1024 * 1024 {
            bail!("PACKAGE_TOO_LARGE");
        }
        let package: Self = serde_json::from_slice(bytes)
            .map_err(|_| anyhow::anyhow!("PACKAGE_CONTRACT_INVALID"))?;
        if package.format != "hrms-tenant-import"
            || package.version != 1
            || package.employees.is_empty()
            || package.employees.len() > 10_000
            || !(1900..=2200).contains(&package.period.year)
            || !(1..=12).contains(&package.period.month)
            || package.tenant_code.trim().is_empty()
            || !valid_hash(&package.source.file_hash)
            || package.source.formula_results != "CACHED_SOURCE_VALUES"
        {
            bail!("PACKAGE_CONTRACT_INVALID");
        }
        let mut codes = HashSet::new();
        let mut source_rows = HashSet::new();
        if !matches!(
            package.salary_effective_policy.as_str(),
            "FIXED_DATE" | "JOINING_DATE"
        ) || (package.salary_effective_policy == "FIXED_DATE")
            != package.salary_effective_from.is_some()
        {
            bail!("SALARY_DATE_POLICY_INVALID");
        }
        crate::configuration::validate(&package.configuration)?;
        for row in &package.employees {
            if !valid_source_ref(&row.source_ref, &package.source.file_hash)
                || !source_rows.insert((row.source_ref.sheet.clone(), row.source_ref.row))
                || row.source_states.values().any(|state| {
                    !matches!(
                        state.as_str(),
                        "MISSING" | "BLANK" | "NA" | "VALUE" | "ERROR"
                    )
                })
            {
                bail!("SOURCE_IDENTITY_INVALID");
            }
            if let Some(code) = &row.employee.code {
                if !codes.insert(code.trim().to_uppercase()) {
                    bail!("DUPLICATE_EMPLOYEE_CODE");
                }
            }
            if row.clear_fields.iter().any(|field| {
                !matches!(
                    field.as_str(),
                    "birth_date"
                        | "confirmation_date"
                        | "exit_date"
                        | "last_working_date"
                        | "gender"
                        | "uan"
                        | "pan"
                        | "aadhaar_last_four"
                        | "bank.account_holder"
                        | "bank.branch"
                        | "bank.account_type"
                )
            }) {
                bail!("EXPLICIT_CLEAR_NOT_SUPPORTED");
            }
            let value = serde_json::to_value(row)?;
            for field in &row.clear_fields {
                let provided = if let Some(bank_field) = field.strip_prefix("bank.") {
                    &value["bank"][bank_field]
                } else if matches!(field.as_str(), "pan" | "aadhaar_last_four") {
                    &value["identity"][field]
                } else {
                    &value["employee"][field]
                };
                if !provided.is_null() {
                    bail!("CLEAR_CONFLICTS_WITH_PROVIDED_VALUE");
                }
            }
            if let Some(period) = &row.period_input {
                if period.year != package.period.year || period.month != package.period.month {
                    bail!("PERIOD_IDENTITY_INVALID");
                }
            }
        }
        if package.issues.iter().any(|issue| {
            !matches!(
                issue.severity.as_str(),
                "WARNING" | "DEFER_SECTION" | "BLOCK_EMPLOYEE" | "BLOCK_TENANT"
            )
        }) {
            bail!("ISSUE_SEVERITY_INVALID");
        }
        if package.issues.iter().any(|issue| {
            issue
                .source_ref
                .as_ref()
                .is_some_and(|source| !valid_source_ref(source, &package.source.file_hash))
                || issue.code.is_empty()
                || issue.code.len() > 100
                || !issue
                    .code
                    .bytes()
                    .all(|c| c.is_ascii_uppercase() || c.is_ascii_digit() || c == b'_')
        }) {
            bail!("ISSUE_SOURCE_INVALID");
        }
        Ok(package)
    }
}

fn fixed_date_policy() -> String {
    "FIXED_DATE".into()
}

fn valid_source_ref(source: &SourceRef, hash: &str) -> bool {
    source.file_hash == hash
        && !source.sheet.trim().is_empty()
        && source.sheet.len() <= 128
        && (1..=1_048_576).contains(&source.row)
        && source.cells.values().all(|cell| {
            let letters = cell.bytes().take_while(|c| c.is_ascii_uppercase()).count();
            (1..=3).contains(&letters)
                && cell
                    .get(letters..)
                    .is_some_and(|row| row == source.row.to_string())
        })
}

pub fn valid_hash(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
