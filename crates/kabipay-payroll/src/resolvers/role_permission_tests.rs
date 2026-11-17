//! Role labels never substitute for exact Payroll permissions and scopes.
use super::*;
use std::collections::HashMap;

fn claims(permission: &str, scope: &str) -> ClientClaims {
    ClientClaims {
        sub: Uuid::new_v4(),
        iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(),
        exp: 0,
        iat: 0,
        tenant_id: Uuid::new_v4(),
        email: String::new(),
        employee_id: Some(Uuid::new_v4()),
        must_change_password: false,
        roles: vec![],
        permissions: vec![permission.into()],
        permission_scopes: HashMap::from([(permission.into(), scope.into())]),
        resource_scopes: HashMap::new(),
    }
}

#[test]
fn custom_role_names_preserve_payroll_permission_and_scope_decisions() {
    for role in ["ADMIN", "HR", "MANAGER", "EMPLOYEE", "New company role"] {
        let mut actor = claims(PERM_PAYROLL_MANAGE, "ALL");
        actor.roles = vec![role.into()];
        assert!(payroll_tenant_all_scope_from_claims(Some(&actor), PERM_PAYROLL_MANAGE).is_ok());
        actor
            .permission_scopes
            .insert(PERM_PAYROLL_MANAGE.into(), "SELF".into());
        assert!(payroll_tenant_all_scope_from_claims(Some(&actor), PERM_PAYROLL_MANAGE).is_err());
        actor
            .permission_scopes
            .insert(PERM_PAYROLL_MANAGE.into(), "ALL".into());
        actor.permissions.clear();
        assert!(payroll_tenant_all_scope_from_claims(Some(&actor), PERM_PAYROLL_MANAGE).is_err());

        for (scope, expected) in [
            ("SELF", ScopeType::Self_),
            ("TEAM", ScopeType::Team),
            ("ALL", ScopeType::All),
        ] {
            let mut reader = claims(PERM_PAYROLL_READ, scope);
            reader.roles = vec![role.into()];
            assert_eq!(
                payroll_read_scope_from_claims(Some(&reader)).expect("valid exact grant"),
                expected
            );
            reader.permission_scopes.clear();
            assert!(payroll_read_scope_from_claims(Some(&reader)).is_err());
        }
    }
}
