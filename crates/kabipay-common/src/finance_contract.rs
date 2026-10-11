//! Private Employee/Payroll coordination. Client JWTs never authorize this transport.
use crate::{context::ClientClaims, KabiPayError, KabiPayResult};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use hmac::{Hmac, Mac};
use serde::{Deserialize, Serialize};
use sha2::Sha256;
use uuid::Uuid;

/// Exact HR authority or JWT-bound employee access; no TEAM/DEPARTMENT widening.
pub fn exit_visibility_employee(claims: &ClientClaims) -> KabiPayResult<Option<Uuid>> {
    use crate::context::ScopeType;
    if claims.has_any_permission(&["onboarding:manage"])
        && claims.scope_for_permission("onboarding:manage") == Some(ScopeType::All)
    {
        return Ok(None);
    }
    if crate::client_data_scope::data_scope_from_claims(Some(claims), "onboarding:self")?
        != ScopeType::Self_
    {
        return Err(KabiPayError::Forbidden(
            "Exit visibility requires company or self scope".into(),
        ));
    }
    claims
        .employee_id
        .filter(|id| !id.is_nil())
        .map(Some)
        .ok_or_else(|| {
            KabiPayError::Forbidden("Exit self access requires a linked employee".into())
        })
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum ServiceCaller {
    Employee,
    Payroll,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum FinanceAction {
    ReadSeparation,
    ReadFundingContext,
    OperationStatus,
    PrepareExitApproval,
    PrepareEmptyExitReview,
    ConfirmExitApproval,
    CancelExitApproval,
    EnsureFnf,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinanceMessage {
    pub version: i32,
    pub caller: ServiceCaller,
    pub recipient: ServiceCaller,
    pub issued_at: i64,
    pub request_id: Uuid,
    pub tenant_id: Uuid,
    pub action: FinanceAction,
    pub client_claims: Option<ClientClaims>,
    pub input: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedFinanceMessage {
    pub message: FinanceMessage,
    pub signature: String,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FinanceResponse {
    pub version: i32,
    pub caller: ServiceCaller,
    pub recipient: ServiceCaller,
    pub tenant_id: Uuid,
    pub request_id: Uuid,
    pub action: FinanceAction,
    pub issued_at: i64,
    pub result: serde_json::Value,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SignedFinanceResponse {
    pub response: FinanceResponse,
    pub signature: String,
}

fn response_mac(response: &FinanceResponse, secret: &[u8]) -> KabiPayResult<Hmac<Sha256>> {
    if secret.len() < 32 {
        return Err(KabiPayError::Unauthorised);
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| KabiPayError::Unauthorised)?;
    mac.update(b"kabipay-private-finance-response-v1\0");
    mac.update(
        &serde_json::to_vec(response)
            .map_err(|_| KabiPayError::Internal("Finance response serialization failed".into()))?,
    );
    Ok(mac)
}

pub fn signed_response(
    request: &FinanceMessage,
    result: serde_json::Value,
    now: i64,
    secret: &[u8],
) -> KabiPayResult<SignedFinanceResponse> {
    let response = FinanceResponse {
        version: 1,
        caller: request.recipient,
        recipient: request.caller,
        tenant_id: request.tenant_id,
        request_id: request.request_id,
        action: request.action,
        issued_at: now,
        result,
    };
    let signature =
        URL_SAFE_NO_PAD.encode(response_mac(&response, secret)?.finalize().into_bytes());
    Ok(SignedFinanceResponse {
        response,
        signature,
    })
}

pub fn verify_response<'a>(
    envelope: &'a SignedFinanceResponse,
    request: &FinanceMessage,
    now: i64,
    secret: &[u8],
) -> KabiPayResult<&'a serde_json::Value> {
    let response = &envelope.response;
    if response.version != 1
        || response.caller != request.recipient
        || response.recipient != request.caller
        || response.tenant_id != request.tenant_id
        || response.request_id != request.request_id
        || response.action != request.action
        || response.issued_at > now.saturating_add(5)
        || now.saturating_sub(response.issued_at) > 60
    {
        return Err(KabiPayError::Unauthorised);
    }
    let tag = URL_SAFE_NO_PAD
        .decode(&envelope.signature)
        .map_err(|_| KabiPayError::Unauthorised)?;
    response_mac(response, secret)?
        .verify_slice(&tag)
        .map_err(|_| KabiPayError::Unauthorised)?;
    Ok(&response.result)
}

fn mac(message: &FinanceMessage, secret: &[u8]) -> KabiPayResult<Hmac<Sha256>> {
    if secret.len() < 32 {
        return Err(KabiPayError::Internal(
            "Configure a dedicated Finance service secret of at least 32 bytes".into(),
        ));
    }
    let mut mac = Hmac::<Sha256>::new_from_slice(secret).map_err(|_| KabiPayError::Unauthorised)?;
    mac.update(b"kabipay-private-finance-v1\0");
    mac.update(
        &serde_json::to_vec(message)
            .map_err(|_| KabiPayError::Internal("Finance message serialization failed".into()))?,
    );
    Ok(mac)
}

pub fn sign_message(message: FinanceMessage, secret: &[u8]) -> KabiPayResult<SignedFinanceMessage> {
    let signature = URL_SAFE_NO_PAD.encode(mac(&message, secret)?.finalize().into_bytes());
    Ok(SignedFinanceMessage { message, signature })
}

pub fn verify_message<'a>(
    envelope: &'a SignedFinanceMessage,
    secret: &[u8],
    recipient: ServiceCaller,
    now: i64,
) -> KabiPayResult<&'a FinanceMessage> {
    let message = &envelope.message;
    let valid_action = match recipient {
        ServiceCaller::Employee => matches!(
            message.action,
            FinanceAction::ReadSeparation
                | FinanceAction::ReadFundingContext
                | FinanceAction::OperationStatus
        ),
        ServiceCaller::Payroll => matches!(
            message.action,
            FinanceAction::PrepareExitApproval
                | FinanceAction::PrepareEmptyExitReview
                | FinanceAction::ConfirmExitApproval
                | FinanceAction::CancelExitApproval
                | FinanceAction::EnsureFnf
        ),
    };
    if message.version != 1
        || message.recipient != recipient
        || message.caller == recipient
        || message.tenant_id.is_nil()
        || message.request_id.is_nil()
        || !valid_action
        || message.issued_at > now.saturating_add(5)
        || now.saturating_sub(message.issued_at) > 60
    {
        return Err(KabiPayError::Unauthorised);
    }
    let tag = URL_SAFE_NO_PAD
        .decode(&envelope.signature)
        .map_err(|_| KabiPayError::Unauthorised)?;
    mac(message, secret)?
        .verify_slice(&tag)
        .map_err(|_| KabiPayError::Unauthorised)?;
    if let Some(claims) = &message.client_claims {
        if claims.tenant_id != message.tenant_id
            || claims.sub.is_nil()
            || claims.iss != crate::context::CLIENT_JWT_ISSUER
            || claims.exp <= now
        {
            return Err(KabiPayError::Unauthorised);
        }
    }
    if matches!(
        message.action,
        FinanceAction::PrepareExitApproval
            | FinanceAction::PrepareEmptyExitReview
            | FinanceAction::EnsureFnf
    ) {
        let claims = message
            .client_claims
            .as_ref()
            .ok_or(KabiPayError::Unauthorised)?;
        if crate::client_data_scope::data_scope_from_claims(Some(claims), "onboarding:manage")?
            != crate::context::ScopeType::All
        {
            return Err(KabiPayError::Forbidden(
                "Exit finance coordination requires ALL HR scope".into(),
            ));
        }
    }
    if message.action == FinanceAction::ReadFundingContext {
        let claims = message
            .client_claims
            .as_ref()
            .ok_or(KabiPayError::Unauthorised)?;
        if crate::client_data_scope::data_scope_from_claims(Some(claims), "loan:disburse")?
            != crate::context::ScopeType::All
        {
            return Err(KabiPayError::Forbidden(
                "Funding status requires ALL disbursement scope".into(),
            ));
        }
    }
    Ok(message)
}

pub fn service_secret() -> KabiPayResult<String> {
    let secret = std::env::var("KABIPAY_FINANCE_SERVICE_SECRET").map_err(|_| {
        KabiPayError::Internal(
            "Configure KABIPAY_FINANCE_SERVICE_SECRET for exit finance coordination".into(),
        )
    })?;
    if secret.len() < 32 {
        return Err(KabiPayError::Internal(
            "Finance service secret must contain at least 32 bytes".into(),
        ));
    }
    Ok(secret)
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum HrOperationState {
    Registered,
    Committed,
    Cancelled,
}

/// Authoritative read model returned only by Employee's private operation endpoint.
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitOperationStatus {
    pub id: Uuid,
    pub separation_id: Uuid,
    pub employee_id: Uuid,
    pub actor_id: Uuid,
    pub review_id: Uuid,
    pub separation_fingerprint: String,
    pub payload_hash: String,
    pub authorization_id: Option<Uuid>,
    pub state: HrOperationState,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitSubject {
    pub separation_id: Uuid,
    pub employee_id: Uuid,
    pub last_working_date: chrono::NaiveDate,
    pub status: String,
    pub fingerprint: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct FundingContext {
    pub employee_id: Uuid,
    pub value_date: chrono::NaiveDate,
    pub pending_separation_id: Option<Uuid>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
pub enum FinanceAuthorizationState {
    Authorized,
    Confirmed,
    Cancelled,
    ReconciliationPending,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
pub struct ExitFinanceAuthorization {
    pub id: Uuid,
    pub operation_id: Uuid,
    pub review_id: Uuid,
    pub employee_id: Uuid,
    pub separation_id: Uuid,
    pub actor_id: Uuid,
    pub payload_hash: String,
    pub state: FinanceAuthorizationState,
}

pub fn reconcile_authorization(
    current: FinanceAuthorizationState,
    operation: Option<HrOperationState>,
) -> KabiPayResult<FinanceAuthorizationState> {
    use FinanceAuthorizationState as F;
    use HrOperationState as H;
    match (current, operation) {
        (F::Confirmed, Some(H::Cancelled)) | (F::Cancelled, Some(H::Committed)) => {
            Err(KabiPayError::Conflict(
                "HR and Finance terminal operation states require review".into(),
            ))
        }
        (F::Confirmed | F::Cancelled, _) => Ok(current),
        (_, Some(H::Committed)) => Ok(F::Confirmed),
        (_, Some(H::Cancelled)) => Ok(F::Cancelled),
        (_, None | Some(H::Registered)) => Ok(F::ReconciliationPending),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_registered_and_lost_replies_never_release_guards() {
        for status in [None, Some(HrOperationState::Registered)] {
            assert_eq!(
                reconcile_authorization(FinanceAuthorizationState::Authorized, status).unwrap(),
                FinanceAuthorizationState::ReconciliationPending
            );
        }
        assert_eq!(
            reconcile_authorization(
                FinanceAuthorizationState::ReconciliationPending,
                Some(HrOperationState::Committed)
            )
            .unwrap(),
            FinanceAuthorizationState::Confirmed
        );
        assert_eq!(
            reconcile_authorization(
                FinanceAuthorizationState::Authorized,
                Some(HrOperationState::Cancelled)
            )
            .unwrap(),
            FinanceAuthorizationState::Cancelled
        );
        assert!(reconcile_authorization(
            FinanceAuthorizationState::Confirmed,
            Some(HrOperationState::Cancelled)
        )
        .is_err());
    }
}
