//! Fixed, authenticated read-only mail policy transport. No redirects, proxies,
//! HTTP downgrade, arbitrary route, retry, response logging or business commands.
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use std::time::Duration;

fn unavailable() -> KabiPayError {
    KabiPayError::Internal("EMAIL_ORGANIZATION_POLICY_UNAVAILABLE".into())
}

pub fn mail_policy_url(base: &str) -> KabiPayResult<reqwest::Url> {
    let mut url = reqwest::Url::parse(base).map_err(|_| unavailable())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
        || url.path() != "/"
    {
        return Err(unavailable());
    }
    url.set_path(EMAIL_ORGANIZATION_POLICY_ROUTE);
    Ok(url)
}

pub async fn current_email_organization_policy(
    request: &EmailOrganizationPolicyRequest,
) -> KabiPayResult<EmailOrganizationPolicy> {
    request.validate()?;
    let base = std::env::var("KABIPAY_ORGANIZATION_POLICY_URL").map_err(|_| unavailable())?;
    let url = mail_policy_url(&base)?;
    let body = serde_json::to_vec(request).map_err(|_| unavailable())?;
    if body.len() > EMAIL_PRIVATE_BODY_LIMIT {
        return Err(unavailable());
    }
    let token = kabipay_organization_security::service_issuer_from_env()
        .map_err(|_| unavailable())?
        .issue(
            EMAIL_ORGANIZATION_POLICY_AUDIENCE,
            request.intent.event.tenant_id,
            EMAIL_ORGANIZATION_POLICY_ROUTE,
            &body,
        )
        .map_err(|_| unavailable())?;
    let client = kabipay_common::organization_http::client(Duration::from_secs(10))
        .map_err(|_| unavailable())?;
    let mut response = client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body)
        .send()
        .await
        .map_err(|_| unavailable())?;
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|size| size > EMAIL_PRIVATE_BODY_LIMIT as u64)
    {
        return Err(unavailable());
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
        if bytes.len() + chunk.len() > EMAIL_PRIVATE_BODY_LIMIT {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    let policy: EmailOrganizationPolicy =
        serde_json::from_slice(&bytes).map_err(|_| unavailable())?;
    if policy.membership_version <= 0
        || !["ALL", "SELF", "TEAM", "DEPARTMENT"].contains(&policy.scope.as_str())
    {
        return Err(unavailable());
    }
    request.validate()?;
    Ok(policy)
}
