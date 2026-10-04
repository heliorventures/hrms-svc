//! Read-only planned section actions. Reconciliation is explicit where domain
//! writers must inspect linked records; preview never simulates writes.
use crate::{
    contract::{ImportPackage, SourceRef},
    options::ImportOptions,
};
use anyhow::Result;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PlannedAction {
    pub source_ref: SourceRef,
    pub section: String,
    pub action: String,
    pub code: String,
}

pub async fn actions<C: ConnectionTrait>(
    db: &C,
    package: &ImportPackage,
    options: &ImportOptions,
    preserved: &[crate::preview::PreservedUser],
    replace: bool,
) -> Result<Vec<PlannedAction>> {
    let readiness = crate::validation::validate(package);
    let mut result = Vec::new();
    if let (Some(value), Some(first)) = (&package.company_payroll_policy, package.employees.first())
    {
        let valid = serde_json::from_value::<
            kabipay_payroll::services::contribution_rules::ContributionPolicy,
        >(value.clone())
        .is_ok_and(|policy| policy.validate().is_ok());
        result.push(PlannedAction {
            source_ref: first.source_ref.clone(),
            section: "company_payroll_policy".into(),
            action: if valid { "RECONCILE" } else { "DEFERRED" }.into(),
            code: if valid {
                "VALIDATE_EXISTING_COMPANY_POLICY"
            } else {
                "COMPANY_POLICY_REVIEW_REQUIRED"
            }
            .into(),
        });
    }
    for (row, ready) in package.employees.iter().zip(readiness) {
        let mut existing = None;
        let mut ambiguous = false;
        if let Some(code) = &row.employee.code {
            let found=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
                "SELECT id,user_id,is_deleted,first_name,last_name,date_of_joining FROM employee WHERE tenant_id=$1 AND lower(employee_code)=lower($2)",
                [options.tenant_id.into(),code.clone().into()])).await?;
            ambiguous = found.len() > 1
                || found
                    .first()
                    .is_some_and(|value| value.try_get::<bool>("", "is_deleted").unwrap_or(true));
            if found.len() == 1 && !ambiguous {
                existing = found.into_iter().next();
            }
        }
        let blocked = !ready.core_ready || ambiguous;
        let retained = existing.as_ref().is_some_and(|value| {
            value
                .try_get::<Uuid>("", "id")
                .ok()
                .is_some_and(|id| preserved.iter().any(|user| user.employee_id == Some(id)))
        });
        // Replacement preserves explicitly selected identities; its manifest is
        // authoritative. Existing ordinary staff will be recreated after reset.
        let core_action = if blocked {
            "DEFERRED"
        } else if replace && !retained {
            "CREATE_AFTER_RESET"
        } else if let Some(value) = &existing {
            let same = value.try_get::<String>("", "first_name")?.as_str()
                == row.employee.first_name.as_deref().unwrap_or("")
                && value.try_get::<String>("", "last_name")?.as_str()
                    == row.employee.last_name.as_deref().unwrap_or("")
                && Some(value.try_get::<chrono::NaiveDate>("", "date_of_joining")?)
                    == row.employee.joining_date;
            if same {
                "UNCHANGED"
            } else {
                "UPDATE"
            }
        } else {
            "CREATE"
        };
        let core_code = if ambiguous {
            "EMPLOYEE_IDENTITY_REVIEW_REQUIRED"
        } else if blocked {
            "CORE_IDENTITY_UNRESOLVED"
        } else {
            "CORE_READY"
        };
        result.push(PlannedAction {
            source_ref: row.source_ref.clone(),
            section: "employee".into(),
            action: core_action.into(),
            code: core_code.into(),
        });
        let linked_login = existing.as_ref().is_some_and(|value| {
            value
                .try_get::<Option<Uuid>>("", "user_id")
                .ok()
                .flatten()
                .is_some()
        }) && (!replace || retained);
        let login_supplied = row
            .employee
            .code
            .as_ref()
            .is_some_and(|code| options.login_by_employee_code.contains_key(code));
        let (login_action, login_code) = if blocked {
            ("DEFERRED", "CORE_IDENTITY_UNRESOLVED")
        } else if linked_login {
            ("RECONCILE", "VALIDATE_EXISTING_LOGIN_LINK")
        } else if login_supplied {
            ("CREATE", "VALIDATE_REVIEWED_LOGIN_MANIFEST")
        } else {
            ("DEFERRED", "LOGIN_MANIFEST_NOT_SUPPLIED")
        };
        result.push(PlannedAction {
            source_ref: row.source_ref.clone(),
            section: "login".into(),
            action: login_action.into(),
            code: login_code.into(),
        });
        for (section, supplied, valid) in [
            (
                "tax_settings",
                row.tax_settings.is_some(),
                crate::tax_import::settings_valid(row.tax_settings.as_ref()).is_ok(),
            ),
            (
                "tax_history",
                !row.tax_history.is_empty(),
                crate::tax_import::history_valid(&row.tax_history).is_ok(),
            ),
            ("profile", true, true),
            ("department", row.employee.department.is_some(), true),
            ("designation", row.employee.designation.is_some(), true),
            (
                "identity",
                row.identity.is_some()
                    || row
                        .clear_fields
                        .iter()
                        .any(|f| matches!(f.as_str(), "pan" | "aadhaar_last_four")),
                true,
            ),
            (
                "bank",
                row.bank.is_some() || row.clear_fields.iter().any(|f| f.starts_with("bank.")),
                true,
            ),
            (
                "recurring_salary",
                row.recurring_salary.is_some(),
                ready.salary_ready,
            ),
            (
                "leave_opening",
                row.leave_opening.is_some(),
                ready.leave_ready,
            ),
            (
                "period_input",
                row.period_input.is_some(),
                ready.period_ready,
            ),
        ] {
            let (action, code) = if blocked {
                ("DEFERRED", "CORE_IDENTITY_UNRESOLVED")
            } else if !supplied {
                ("DEFERRED", "SOURCE_SECTION_NOT_SUPPLIED")
            } else if !valid {
                ("STAGE_FOR_REVIEW", "SECTION_REVIEW_REQUIRED")
            } else {
                ("RECONCILE", "SHARED_DOMAIN_WRITER_VALIDATES_TARGET")
            };
            result.push(PlannedAction {
                source_ref: row.source_ref.clone(),
                section: section.into(),
                action: action.into(),
                code: code.into(),
            });
        }
        if row
            .recurring_salary
            .as_ref()
            .is_some_and(|salary| salary["annual_employer_pf"].is_null())
        {
            result.push(PlannedAction {
                source_ref: row.source_ref.clone(),
                section: "employer_cost".into(),
                action: "DEFERRED".into(),
                code: "EMPLOYER_COST_RULE_UNRESOLVED".into(),
            });
        }
    }
    Ok(result)
}
