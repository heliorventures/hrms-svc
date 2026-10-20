use crate::models::{EmployeeScope, RoleCapability};
use kabipay_common::{KabiPayError, KabiPayResult};
pub const MANAGE: &str = "organization:manage";
pub const DELEGATE: &str = "organization:delegate";
pub const CATALOG: &[&str] = &[
    MANAGE,
    DELEGATE,
    "employee:read",
    "employee:manage",
    "role:manage",
    "workflow:manage",
    "onboarding:manage",
    "leave:read",
    "leave:approve",
    "leave:manage",
    "timesheet:read",
    "timesheet:approve",
    "timesheet:manage",
    "performance:read",
    "performance:evaluate",
    "performance:manage",
    "payroll:read",
    "payroll:manage",
    "analytics:read",
    "history:read",
    "history:export",
    "feedback:write",
];
pub fn validate_capabilities(capabilities: &[RoleCapability]) -> KabiPayResult<bool> {
    if capabilities.is_empty() || capabilities.len() > CATALOG.len() {
        return Err(KabiPayError::Validation(
            "role capabilities are required and bounded".into(),
        ));
    }
    let mut seen = std::collections::HashSet::new();
    let mut central_only = true;
    for capability in capabilities {
        if !CATALOG.contains(&capability.code.as_str()) || !seen.insert(&capability.code) {
            return Err(KabiPayError::Validation(
                "unsupported or duplicate capability".into(),
            ));
        }
        let central = matches!(capability.code.as_str(), MANAGE | DELEGATE);
        if central && capability.employee_scope != EmployeeScope::All {
            return Err(KabiPayError::Validation(
                "organization administration requires ALL metadata scope".into(),
            ));
        }
        central_only &= central;
    }
    Ok(central_only)
}
#[cfg(test)]
mod tests {
    use super::*;
    fn cap(code: &str, scope: EmployeeScope) -> RoleCapability {
        RoleCapability {
            code: code.into(),
            employee_scope: scope,
        }
    }
    #[test]
    fn explicit_catalog_and_scopes_do_not_turn_metadata_administration_into_business_access() {
        assert!(validate_capabilities(&[cap(MANAGE, EmployeeScope::All)]).unwrap());
        assert!(!validate_capabilities(&[cap("payroll:manage", EmployeeScope::All)]).unwrap());
        assert!(!validate_capabilities(&[
            cap(MANAGE, EmployeeScope::All),
            cap("leave:approve", EmployeeScope::Team)
        ])
        .unwrap());
        for caps in [
            vec![],
            vec![cap("payroll:*", EmployeeScope::All)],
            vec![cap(MANAGE, EmployeeScope::Team)],
            vec![
                cap(MANAGE, EmployeeScope::All),
                cap(MANAGE, EmployeeScope::All),
            ],
        ] {
            assert!(validate_capabilities(&caps).is_err());
        }
    }
}
