//! Scope choices accepted by the existing action boundaries. Unknown catalog
//! actions remain visible but cannot be activated until their policy is defined.
use std::collections::HashSet;

pub fn supported_scope_types(resource: &str, action: &str) -> &'static [&'static str] {
    match (resource, action) {
        (
            "employee" | "attendance" | "timesheet" | "leave" | "expense" | "travel" | "payroll"
            | "tax" | "notification" | "assets" | "loan",
            "read",
        ) => &["SELF", "TEAM", "DEPARTMENT", "ALL"],
        ("attendance", "regularize")
        | ("leave" | "expense" | "travel" | "tax" | "timesheet", "approve") => &["TEAM", "ALL"],
        ("survey", "results") | ("loan", "approve") => &["TEAM", "DEPARTMENT", "ALL"],
        ("loan", "submit") => &["SELF", "ALL"],
        ("performance", "evaluate") => &["TEAM"],
        ("attendance", "punch_self")
        | ("timesheet", "write")
        | ("leave" | "expense" | "travel" | "tax", "submit")
        | ("assets" | "benefits" | "grievance" | "onboarding" | "performance", "self")
        | ("survey", "respond") => &["SELF"],
        ("employee_directory" | "analytics", "read")
        | ("employee", "write" | "manage")
        | ("attendance", "punch_policy")
        | ("payroll", "statutory_export")
        | ("expense", "pay")
        | ("prejoining", "review")
        | ("notification", "create" | "update" | "delete")
        | ("loan", "disburse" | "repay" | "policy" | "correct" | "exit-review" | "export")
        | (
            "assets" | "benefits" | "compensation" | "expense" | "grievance" | "leave" | "learning"
            | "loan" | "notification" | "onboarding" | "payroll" | "performance" | "prejoining"
            | "recruitment" | "role" | "succession" | "survey" | "tax" | "timesheet" | "travel"
            | "workflow",
            "manage",
        ) => &["ALL"],
        _ => &[],
    }
}

/// Only a grant that was already unscoped may remain inactive. Removing a
/// configured scope or adding a permission requires a deliberate supported scope.
pub fn validate_access(
    permissions: &HashSet<String>,
    scopes: &[(String, String, String)],
    retained_inactive: &HashSet<String>,
) -> Result<(), String> {
    let mut scoped = HashSet::new();
    for (resource, action, scope) in scopes {
        let key = format!("{resource}:{action}");
        if !permissions.contains(&key) {
            return Err(format!("Scope {key} requires its permission grant"));
        }
        if !scoped.insert(key.clone()) {
            return Err(format!("Only one exact scope is allowed for {key}"));
        }
        if !supported_scope_types(resource, action).contains(&scope.as_str()) {
            return Err(format!("Scope {scope} is not supported for {key}"));
        }
    }
    for key in permissions {
        if !scoped.contains(key) && !retained_inactive.contains(key) {
            return Err(format!("Select a supported scope for {key}"));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn keys(values: &[&str]) -> HashSet<String> {
        values.iter().map(|value| value.to_string()).collect()
    }
    fn scope(resource: &str, action: &str, scope: &str) -> (String, String, String) {
        (resource.into(), action.into(), scope.into())
    }

    #[test]
    fn grants_need_exact_supported_scope_without_role_name_fallback() {
        let selected = keys(&["role:manage"]);
        assert!(validate_access(&selected, &[], &HashSet::new()).is_err());
        for invalid in ["SELF", "TEAM", "DEPARTMENT", "invalid", ""] {
            assert!(validate_access(
                &selected,
                &[scope("role", "manage", invalid)],
                &HashSet::new()
            )
            .is_err());
        }
        assert!(validate_access(
            &selected,
            &[scope("role", "manage", "ALL")],
            &HashSet::new()
        )
        .is_ok());
    }
    #[test]
    fn legacy_inactive_extras_are_retained_without_activating_loans() {
        let selected = keys(&["loan:read", "loan:manage"]);
        assert!(validate_access(&selected, &[], &selected).is_ok());
        assert!(validate_access(&selected, &[scope("loan", "read", "SELF")], &selected).is_ok());
        assert!(validate_access(&selected, &[scope("loan", "manage", "TEAM")], &selected).is_err());
        assert!(validate_access(&keys(&["loan:submit"]), &[], &selected).is_err());
    }
    #[test]
    fn orphan_duplicate_and_unknown_actions_fail_closed() {
        let selected = keys(&["assets:read"]);
        assert!(validate_access(
            &selected,
            &[scope("role", "manage", "ALL")],
            &HashSet::new()
        )
        .is_err());
        assert!(validate_access(
            &selected,
            &[
                scope("assets", "read", "TEAM"),
                scope("assets", "read", "ALL")
            ],
            &HashSet::new()
        )
        .is_err());
        assert!(validate_access(
            &keys(&["custom:manage"]),
            &[scope("custom", "manage", "ALL")],
            &HashSet::new()
        )
        .is_err());
        for value in supported_scope_types("assets", "read") {
            assert!(validate_access(
                &selected,
                &[scope("assets", "read", value)],
                &HashSet::new()
            )
            .is_ok());
        }
    }
    #[test]
    fn existing_self_and_hr_admin_contracts_stay_explicit() {
        assert_eq!(supported_scope_types("performance", "evaluate"), &["TEAM"]);
        assert_eq!(supported_scope_types("timesheet", "write"), &["SELF"]);
        assert_eq!(supported_scope_types("employee", "write"), &["ALL"]);
        assert_eq!(supported_scope_types("payroll", "manage"), &["ALL"]);
        assert_eq!(supported_scope_types("tax", "approve"), &["TEAM", "ALL"]);
        assert_eq!(supported_scope_types("loan", "submit"), &["SELF", "ALL"]);
        assert_eq!(
            supported_scope_types("loan", "approve"),
            &["TEAM", "DEPARTMENT", "ALL"]
        );
        assert_eq!(
            supported_scope_types("employee_directory", "read"),
            &["ALL"]
        );
    }
}
