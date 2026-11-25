//! Fixed HTTPS mail-only routes, independent of business command transport.
//! No caller supplies a URL, redirect, proxy, mailbox, SQL or recipient token.
use super::store::DeliveryAttempt;
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use serde::Deserialize;
use std::{collections::BTreeMap, path::Path, time::Duration};
use tokio::io::AsyncReadExt;
use uuid::Uuid;
fn invalid() -> KabiPayError {
    KabiPayError::Internal("EMAIL_ROUTING_UNAVAILABLE".into())
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Config {
    version: u8,
    companies: Vec<Company>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Company {
    tenant_id: Uuid,
    recipient_origin: String,
    domains: Vec<Domain>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Domain {
    domain: String,
    origin: String,
}
struct Routes {
    recipient: reqwest::Url,
    domains: BTreeMap<String, reqwest::Url>,
}
pub struct EmailRoutes {
    companies: BTreeMap<Uuid, Routes>,
}
impl std::fmt::Debug for EmailRoutes {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("EmailRoutes([fixed private endpoints])")
    }
}
fn root(origin: &str) -> KabiPayResult<reqwest::Url> {
    let url = reqwest::Url::parse(origin).map_err(|_| invalid())?;
    if url.scheme() != "https"
        || url.host_str().is_none()
        || !url.username().is_empty()
        || url.password().is_some()
        || url.path() != "/"
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(invalid());
    }
    Ok(url)
}
impl EmailRoutes {
    pub fn from_json(bytes: &[u8]) -> KabiPayResult<Self> {
        if bytes.len() > 1024 * 1024 {
            return Err(invalid());
        }
        let config: Config = serde_json::from_slice(bytes).map_err(|_| invalid())?;
        if config.version != 1 || config.companies.is_empty() || config.companies.len() > 100 {
            return Err(invalid());
        }
        let mut companies = BTreeMap::new();
        for company in config.companies {
            if company.tenant_id.is_nil() || company.domains.len() > 7 {
                return Err(invalid());
            }
            let mut domains = BTreeMap::new();
            for domain in company.domains {
                if ![
                    "leave",
                    "expense",
                    "employee",
                    "attendance",
                    "payroll",
                    "tax",
                    "performance",
                ]
                .contains(&domain.domain.as_str())
                    || domains
                        .insert(domain.domain, root(&domain.origin)?)
                        .is_some()
                {
                    return Err(invalid());
                }
            }
            if companies
                .insert(
                    company.tenant_id,
                    Routes {
                        recipient: root(&company.recipient_origin)?,
                        domains,
                    },
                )
                .is_some()
            {
                return Err(invalid());
            }
        }
        Ok(Self { companies })
    }
    pub async fn configured() -> KabiPayResult<Self> {
        let path = std::env::var_os("KABIPAY_EMAIL_ROUTES_FILE").ok_or_else(invalid)?;
        if !Path::new(&path).is_absolute() {
            return Err(invalid());
        }
        let file = tokio::fs::File::open(path).await.map_err(|_| invalid())?;
        if !file.metadata().await.map_err(|_| invalid())?.is_file() {
            return Err(invalid());
        }
        let mut bytes = Vec::new();
        file.take(1024 * 1024 + 1)
            .read_to_end(&mut bytes)
            .await
            .map_err(|_| invalid())?;
        Self::from_json(&bytes)
    }
    pub fn eligibility(&self, tenant: Uuid, domain: &str) -> KabiPayResult<reqwest::Url> {
        let mut url = self
            .companies
            .get(&tenant)
            .and_then(|r| r.domains.get(domain))
            .ok_or_else(invalid)?
            .clone();
        url.set_path(EMAIL_ELIGIBILITY_ROUTE);
        Ok(url)
    }
    fn recipient(&self, tenant: Uuid) -> KabiPayResult<reqwest::Url> {
        let mut url = self
            .companies
            .get(&tenant)
            .ok_or_else(invalid)?
            .recipient
            .clone();
        url.set_path(EMAIL_RECIPIENT_ROUTE);
        Ok(url)
    }
    pub async fn resolve(
        &self,
        attempt: &DeliveryAttempt,
    ) -> Result<DeliveryRecipient, RouteFailure> {
        let request = EmailEligibilityRequest {
            intent_id: attempt.id,
            event_hash: attempt.event_hash.clone(),
            event: attempt.event.clone(),
        };
        request.validate().map_err(|_| RouteFailure::Obsolete)?;
        let body = serde_json::to_vec(&request).map_err(|_| RouteFailure::Unavailable)?;
        let issuer = kabipay_organization_security::service_issuer_from_env()
            .map_err(|_| RouteFailure::Unavailable)?;
        let proof = if request.event.kind == EmailEventKind::Test {
            issuer
                .issue(
                    EMAIL_RECIPIENT_AUDIENCE,
                    attempt.tenant,
                    EMAIL_RECIPIENT_ROUTE,
                    &body,
                )
                .map_err(|_| RouteFailure::Unavailable)?
        } else {
            let token = issuer
                .issue(
                    EMAIL_ELIGIBILITY_AUDIENCE,
                    attempt.tenant,
                    EMAIL_ELIGIBILITY_ROUTE,
                    &body,
                )
                .map_err(|_| RouteFailure::Unavailable)?;
            match post::<EmailEligibility>(
                self.eligibility(attempt.tenant, request.event.producer())
                    .map_err(|_| RouteFailure::Unavailable)?,
                &token,
                &body,
            )
            .await?
            {
                EmailEligibility::Eligible { recipient_proof } => recipient_proof,
                EmailEligibility::Obsolete => return Err(RouteFailure::Obsolete),
                EmailEligibility::Forbidden => return Err(RouteFailure::Forbidden),
            }
        };
        let source = match request.event.recipient {
            EmailRecipientRef::LocalUser { .. } => attempt.tenant,
            EmailRecipientRef::OrganizationPrincipal {
                source_tenant_id, ..
            } => source_tenant_id,
        };
        let resolution = post::<EmailRecipientResolution>(
            self.recipient(source)
                .map_err(|_| RouteFailure::Unavailable)?,
            &proof,
            &body,
        )
        .await?;
        let result = match resolution {
            EmailRecipientResolution::Recipient { recipient }
                if request.event.kind != EmailEventKind::Test =>
            {
                DeliveryRecipient::Business(recipient)
            }
            EmailRecipientResolution::TestAuthorized { identity }
                if request.event.kind == EmailEventKind::Test =>
            {
                DeliveryRecipient::Test(identity)
            }
            EmailRecipientResolution::Unavailable => {
                return Err(RouteFailure::RecipientUnavailable)
            }
            EmailRecipientResolution::Forbidden => return Err(RouteFailure::Forbidden),
            _ => return Err(RouteFailure::Unavailable),
        };
        let identity = result.identity();
        if identity.source_tenant_id != source
            || matches!(request.event.recipient,EmailRecipientRef::LocalUser{user_id} if identity.source_user_id!=user_id)
        {
            return Err(RouteFailure::Unavailable);
        }
        Ok(result)
    }
}
#[derive(Clone, Copy, Debug)]
pub enum RouteFailure {
    Unavailable,
    Obsolete,
    Forbidden,
    RecipientUnavailable,
}
pub enum DeliveryRecipient {
    Business(PrivateEmailRecipient),
    Test(EmailRecipientIdentity),
}
impl DeliveryRecipient {
    pub fn identity(&self) -> EmailRecipientIdentity {
        match self {
            Self::Business(recipient) => recipient.identity,
            Self::Test(identity) => *identity,
        }
    }
    pub fn unchanged(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Business(a), Self::Business(b)) => {
                a.identity == b.identity
                    && a.address() == b.address()
                    && a.source_login_origin() == b.source_login_origin()
            }
            (Self::Test(a), Self::Test(b)) => a == b,
            _ => false,
        }
    }
}
impl std::fmt::Debug for DeliveryRecipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("DeliveryRecipient([redacted])")
    }
}
async fn post<R: serde::de::DeserializeOwned>(
    url: reqwest::Url,
    token: &str,
    body: &[u8],
) -> Result<R, RouteFailure> {
    if body.len() > EMAIL_PRIVATE_BODY_LIMIT {
        return Err(RouteFailure::Unavailable);
    }
    let client = kabipay_common::organization_http::client(Duration::from_secs(8))
        .map_err(|_| RouteFailure::Unavailable)?;
    let mut response = client
        .post(url)
        .bearer_auth(token)
        .header("content-type", "application/json")
        .body(body.to_vec())
        .send()
        .await
        .map_err(|_| RouteFailure::Unavailable)?;
    // Authentication/configuration errors are transient operational failures, not
    // falsely classified as a missing recipient. Known revocation is typed JSON.
    if !response.status().is_success()
        || response
            .content_length()
            .is_some_and(|n| n > EMAIL_PRIVATE_BODY_LIMIT as u64)
    {
        return Err(RouteFailure::Unavailable);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| RouteFailure::Unavailable)?
    {
        if bytes.len() + chunk.len() > EMAIL_PRIVATE_BODY_LIMIT {
            return Err(RouteFailure::Unavailable);
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| RouteFailure::Unavailable)
}
