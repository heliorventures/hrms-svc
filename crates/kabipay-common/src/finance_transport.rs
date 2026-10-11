//! Bounded private transport; no client-selected URL, redirects, or implicit retries.
use super::finance_contract::{self, FinanceAction, FinanceMessage, ServiceCaller};
use crate::{context::ClientClaims, KabiPayError, KabiPayResult};
use std::time::Duration;
use uuid::Uuid;

pub async fn send(
    caller: ServiceCaller,
    recipient: ServiceCaller,
    tenant: Uuid,
    claims: Option<&ClientClaims>,
    action: FinanceAction,
    input: serde_json::Value,
) -> KabiPayResult<serde_json::Value> {
    let variable = match recipient {
        ServiceCaller::Employee => "KABIPAY_FINANCE_EMPLOYEE_URL",
        ServiceCaller::Payroll => "KABIPAY_FINANCE_PAYROLL_URL",
    };
    let configured = std::env::var(variable).map_err(|_| unavailable())?;
    let mut url = reqwest::Url::parse(&configured).map_err(|_| unavailable())?;
    if !matches!(url.scheme(), "http" | "https")
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || !matches!(url.path(), "" | "/")
    {
        return Err(unavailable());
    }
    url.set_path("/internal/finance");
    let secret = finance_contract::service_secret().map_err(|_| unavailable())?;
    let envelope = finance_contract::sign_message(
        FinanceMessage {
            version: 1,
            caller,
            recipient,
            tenant_id: tenant,
            issued_at: chrono::Utc::now().timestamp(),
            request_id: Uuid::new_v4(),
            action,
            client_claims: claims.cloned(),
            input,
        },
        secret.as_bytes(),
    )?;
    let body = serde_json::to_vec(&envelope).map_err(|_| unavailable())?;
    if body.len() > 131072 {
        return Err(KabiPayError::Validation(
            "Finance request is too large".into(),
        ));
    }
    let client = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(10))
        .redirect(reqwest::redirect::Policy::none())
        .build()
        .map_err(|_| unavailable())?;
    let mut response = client
        .post(url)
        .header("Content-Type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success() {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len().saturating_add(chunk.len()) > 1048576 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let response = serde_json::from_slice::<finance_contract::SignedFinanceResponse>(&bytes)
        .map_err(|_| unavailable())?;
    finance_contract::verify_response(
        &response,
        &envelope.message,
        chrono::Utc::now().timestamp(),
        secret.as_bytes(),
    )
    .cloned()
    .map_err(|_| unavailable())
}

fn unavailable() -> KabiPayError {
    KabiPayError::Conflict(
        "Finance coordination is unavailable or requires review; retry the recorded operation"
            .into(),
    )
}
