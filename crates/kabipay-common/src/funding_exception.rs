//! Company authority for the explicitly reviewed pending-resignation funding exception.
use crate::{
    context::{ClientClaims, ScopeType},
    KabiPayError, KabiPayResult,
};

pub fn validate(claims: &ClientClaims, pending: bool, reason: Option<&str>) -> KabiPayResult<()> {
    if pending && reason.is_none() {
        return Err(KabiPayError::Validation(
            "A resignation is pending. HR/Admin must record a funding exception reason".into(),
        ));
    }
    if let Some(reason) = reason {
        if reason.trim().is_empty() || reason.chars().count() > 2000 {
            return Err(KabiPayError::Validation(
                "Funding exception reason is required and must not exceed 2000 characters".into(),
            ));
        }
        if !claims.has_any_permission(&["onboarding:manage"])
            || claims.scope_for_permission("onboarding:manage") != Some(ScopeType::All)
        {
            return Err(KabiPayError::Forbidden(
                "Funding exceptions require company HR/Admin authority".into(),
            ));
        }
    }
    Ok(())
}
