//! Imported-bank/profile reads use exact grants, not built-in role labels.
use super::*;
use std::collections::HashMap;

#[test]
fn custom_role_names_preserve_private_profile_permission_and_target_checks() {
    let employee = Uuid::new_v4();
    let mut actor = kabipay_common::context::ClientClaims {
        sub: Uuid::new_v4(),
        iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(),
        exp: 0,
        iat: 0,
        tenant_id: Uuid::new_v4(),
        email: String::new(),
        employee_id: Some(employee),
        must_change_password: false,
        roles: vec![],
        permissions: vec![PERM_EMPLOYEE_READ.into()],
        permission_scopes: HashMap::from([(PERM_EMPLOYEE_READ.into(), "SELF".into())]),
        resource_scopes: HashMap::new(),
    };
    for role in ["ADMIN", "HR", "MANAGER", "EMPLOYEE", "New company role"] {
        actor.roles = vec![role.into()];
        actor.permissions = vec![PERM_EMPLOYEE_READ.into()];
        actor.permission_scopes = HashMap::from([(PERM_EMPLOYEE_READ.into(), "SELF".into())]);
        assert_eq!(
            employee_target_access_from_claims(&actor, employee),
            Some(EmployeeTargetAccess::SelfBound)
        );
        actor.permissions.clear();
        assert_eq!(employee_target_access_from_claims(&actor, employee), None);
        actor.permissions = vec![PERM_EMPLOYEE_READ.into()];
        actor.permission_scopes.clear();
        assert_eq!(employee_target_access_from_claims(&actor, employee), None);
        actor
            .permission_scopes
            .insert(PERM_EMPLOYEE_READ.into(), "INVALID".into());
        assert_eq!(employee_target_access_from_claims(&actor, employee), None);
        actor
            .permission_scopes
            .insert(PERM_EMPLOYEE_READ.into(), "TEAM".into());
        // This is only an access candidate. The production loader separately
        // verifies that a different target belongs to the actor's actual team.
        assert_eq!(
            employee_target_access_from_claims(&actor, Uuid::new_v4()),
            Some(EmployeeTargetAccess::ScopedRead)
        );
    }
}
