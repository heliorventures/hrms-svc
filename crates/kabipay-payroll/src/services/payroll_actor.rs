//! Verified payroll attribution shared by local and organization execution.
use kabipay_common::{context::ClientClaims, KabiPayResult};
use kabipay_loans::LoanActorScope;
use uuid::Uuid;
pub struct PayrollActor {
    pub(crate) tenant: Uuid,
    pub(crate) local_user: Option<Uuid>,
    pub(crate) principal: Option<Uuid>,
    pub(crate) loans: LoanActorScope,
    pub(crate) business_date: chrono::NaiveDate,
}
impl PayrollActor {
    pub fn local(
        claims: &ClientClaims,
        tenant: Uuid,
        entitlements: &kabipay_common::entitlements::Entitlements,
    ) -> KabiPayResult<Self> {
        Ok(Self {
            tenant,
            local_user: Some(claims.sub),
            principal: None,
            loans: super::loan_recovery::actor(claims, tenant)?
                .with_tenant_entitlements(entitlements.clone())
                .map_err(|error| match error {
                    kabipay_loans::LoanModuleError::Authority(error) => error,
                    _ => kabipay_common::KabiPayError::Unauthorised,
                })?,
            business_date: entitlements.business_date(),
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
            business_date: proof.entitlements().business_date(),
        }
    }
}
