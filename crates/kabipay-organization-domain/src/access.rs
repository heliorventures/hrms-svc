use crate::{
    Capability, EffectivePeriod, MembershipId, OrganizationError, OrganizationId, PrincipalId,
    ResourceVersion, TenantId,
};
use chrono::{DateTime, Utc};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EmployeeScope {
    SelfOnly,
    Team,
    Department,
    All,
}

/// Trusted stored policy data. This is deliberately not a browser-deserializable claim.
#[derive(Debug, Clone)]
pub struct CompanyGrant {
    pub organization_id: OrganizationId,
    pub membership_id: MembershipId,
    pub tenant_id: TenantId,
    pub principal_id: PrincipalId,
    pub capability: Capability,
    pub scope: EmployeeScope,
    pub period: EffectivePeriod,
    pub version: ResourceVersion,
    pub active: bool,
}
pub struct GrantRequest<'a> {
    pub organization_id: OrganizationId,
    pub membership_id: MembershipId,
    pub tenant_id: TenantId,
    pub principal_id: PrincipalId,
    pub capability: &'a Capability,
}
/// Must be loaded from trusted current policy and the target's accepted fence.
pub struct GrantSecurityState {
    pub organization_active: bool,
    pub membership_active: bool,
    pub identity_eligible: bool,
    pub shared_version: ResourceVersion,
    pub accepted_version: ResourceVersion,
}
impl CompanyGrant {
    /// Resolve only the explicitly granted company/capability and return its employee scope.
    /// Callers must apply that scope and field policy to the actual resource, and check
    /// current target-domain/workflow rules before reading or committing anything.
    ///
    /// # Errors
    /// Denies inactive identities/memberships, expired bindings, stale accepted versions,
    /// and any mismatch in person, organization, membership, company or capability.
    pub fn authorize(
        &self,
        request: &GrantRequest<'_>,
        security: &GrantSecurityState,
        at: DateTime<Utc>,
    ) -> Result<EmployeeScope, OrganizationError> {
        if !self.active
            || !security.organization_active
            || !security.membership_active
            || !security.identity_eligible
            || !self.period.contains(at)
            || self.version != security.shared_version
            || self.version != security.accepted_version
        {
            return Err(OrganizationError::AuthorityUnavailable);
        }
        if self.organization_id != request.organization_id
            || self.membership_id != request.membership_id
            || self.tenant_id != request.tenant_id
            || self.principal_id != request.principal_id
            || &self.capability != request.capability
        {
            return Err(OrganizationError::OutsideGrant);
        }
        Ok(self.scope)
    }
}
