use crate::{LoanModuleError, LoanResult};
use kabipay_common::context::{ClientClaims, ScopeType};
use uuid::Uuid;
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LoanPermission {
    Read,
    Submit,
    Approve,
    Disburse,
    Repay,
    Manage,
    Policy,
    Correct,
    ExitReview,
    Export,
    PayrollRecovery,
    FnfRecovery,
}
impl LoanPermission {
    pub fn wire(self) -> &'static str {
        match self {
            Self::Read => "loan:read",
            Self::Submit => "loan:submit",
            Self::Approve => "loan:approve",
            Self::Disburse => "loan:disburse",
            Self::Repay => "loan:repay",
            Self::Manage => "loan:manage",
            Self::Policy => "loan:policy",
            Self::Correct => "loan:correct",
            Self::ExitReview => "loan:exit-review",
            Self::Export => "loan:export",
            Self::PayrollRecovery => "payroll:manage",
            Self::FnfRecovery => "onboarding:manage",
        }
    }
}
/// Constructed at the authenticated service boundary, never deserialized from browser input.
#[derive(Clone, Debug)]
pub struct LoanActorScope {
    pub(crate) tenant_id: Uuid,
    pub(crate) user_id: Uuid,
    pub(crate) permission: LoanPermission,
    pub(crate) scope: ScopeType,
}
impl LoanActorScope {
    pub fn from_verified_claims(
        claims: &ClientClaims,
        tenant: Uuid,
        permission: LoanPermission,
    ) -> LoanResult<Self> {
        if tenant.is_nil()
            || claims.sub.is_nil()
            || claims.tenant_id != tenant
            || claims.iss != kabipay_common::context::CLIENT_JWT_ISSUER
        {
            return Err(LoanModuleError::Forbidden);
        }
        let scope = kabipay_common::client_data_scope::data_scope_from_claims(
            Some(claims),
            permission.wire(),
        )?;
        Ok(Self {
            tenant_id: tenant,
            user_id: claims.sub,
            permission,
            scope,
        })
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    fn claims() -> ClientClaims {
        ClientClaims {
            sub: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            email: String::new(),
            employee_id: None,
            must_change_password: false,
            roles: vec!["ADMIN".into()],
            permissions: vec!["loan:read".into()],
            permission_scopes: HashMap::from([("loan:read".into(), "SELF".into())]),
            resource_scopes: HashMap::new(),
        }
    }
    #[test]
    fn exact_permission_and_tenant_are_required() {
        let c = claims();
        assert!(
            LoanActorScope::from_verified_claims(&c, c.tenant_id, LoanPermission::Read).is_ok()
        );
        assert!(
            LoanActorScope::from_verified_claims(&c, Uuid::new_v4(), LoanPermission::Read).is_err()
        );
        assert!(
            LoanActorScope::from_verified_claims(&c, c.tenant_id, LoanPermission::Approve).is_err()
        );
    }
    #[test]
    fn admin_role_and_resource_scope_cannot_supply_a_missing_exact_scope() {
        let mut c = claims();
        c.permission_scopes.clear();
        c.resource_scopes.insert("loan".into(), "ALL".into());
        assert!(
            LoanActorScope::from_verified_claims(&c, c.tenant_id, LoanPermission::Read).is_err()
        );
    }
}
