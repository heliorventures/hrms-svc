//! Verified payroll attribution shared by local and organization execution.
use kabipay_common::{context::ClientClaims, KabiPayResult};
use kabipay_loans::LoanActorScope;
use uuid::Uuid;
pub struct PayrollActor {
    pub(crate) tenant: Uuid,
    pub(crate) local_user: Option<Uuid>,
    pub(crate) principal: Option<Uuid>,
    pub(crate) loans: LoanActorScope,
    pub(crate) business_date: Option<chrono::NaiveDate>,
}
impl PayrollActor {
    pub fn local(claims: &ClientClaims, tenant: Uuid) -> KabiPayResult<Self> {
        Ok(Self {
            tenant,
            local_user: Some(claims.sub),
            principal: None,
            loans: super::loan_recovery::actor(claims, tenant)?,
            business_date: None,
        })
    }
    pub fn organization(proof: &kabipay_organization_tenant::VerifiedPayrollAuthority<'_>) -> Self {
        Self {
            tenant: proof.tenant_id(),
            local_user: proof.local_user_id(),
            principal: proof
                .local_user_id()
                .is_none()
                .then(|| proof.principal_id()),
            loans: LoanActorScope::from_verified_organization_payroll(proof),
            business_date: Some(proof.entitlements().business_date()),
        }
    }
}
