use kabipay_common::{KabiPayError, KabiPayResult};
use std::collections::HashMap;
use uuid::Uuid;

fn identity_unavailable() -> KabiPayError {
    KabiPayError::Internal("source identity service unavailable".into())
}
fn eligibility_status(status: reqwest::StatusCode) -> KabiPayResult<()> {
    if status.is_success() {
        Ok(())
    } else if status.is_server_error() || status == reqwest::StatusCode::TOO_MANY_REQUESTS {
        Err(identity_unavailable())
    } else {
        Err(KabiPayError::Unauthorised)
    }
}

#[derive(Clone)]
pub struct IdentityClient {
    endpoints: HashMap<Uuid, reqwest::Url>,
    http: reqwest::Client,
}
pub struct SourceIdentity {
    tenant: Uuid,
    user: Uuid,
    name: String,
    scopes: HashMap<String, String>,
    asymmetric: bool,
}
impl SourceIdentity {
    pub(crate) fn eligible_source(tenant: Uuid, user: Uuid, name: String) -> Self {
        Self {
            tenant,
            user,
            name,
            scopes: HashMap::new(),
            asymmetric: true,
        }
    }
    pub fn tenant(&self) -> Uuid {
        self.tenant
    }
    pub fn asymmetric(&self) -> bool {
        self.asymmetric
    }
    pub fn user(&self) -> Uuid {
        self.user
    }
    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn may_accept_company(&self) -> bool {
        self.scopes
            .get("role:manage")
            .is_some_and(|scope| scope == "ALL")
    }
}
impl IdentityClient {
    pub async fn require_employment_eligible(
        &self,
        tenant: Uuid,
        employee: Uuid,
        expected_user_id: Option<Uuid>,
        issuer: &kabipay_organization_security::ServiceCredentialIssuer,
    ) -> KabiPayResult<()> {
        let mut url = self
            .endpoints
            .get(&tenant)
            .ok_or(KabiPayError::Unauthorised)?
            .clone();
        url.set_path("/auth/internal/employment-eligibility");
        let nonce = Uuid::new_v4();
        let body = serde_json::to_vec(
            &kabipay_organization_protocol::EmploymentEligibilityRequest {
                employee_id: employee,
                expected_user_id,
                nonce,
            },
        )?;
        let token = issuer
            .issue(
                "kabipay-auth",
                tenant,
                "/auth/internal/employment-eligibility",
                &body,
            )
            .map_err(|_| KabiPayError::Unauthorised)?;
        let mut response = self
            .http
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| identity_unavailable())?;
        eligibility_status(response.status())?;
        let mut bytes = Vec::new();
        while let Some(c) = response.chunk().await.map_err(|_| identity_unavailable())? {
            if bytes.len() + c.len() > 4096 {
                return Err(KabiPayError::Unauthorised);
            }
            bytes.extend_from_slice(&c);
        }
        let r: kabipay_organization_protocol::EmploymentEligibilityResponse =
            serde_json::from_slice(&bytes).map_err(|_| KabiPayError::Unauthorised)?;
        if r.tenant_id != tenant || r.employee_id != employee || r.nonce != nonce || !r.eligible {
            return Err(KabiPayError::Unauthorised);
        }
        Ok(())
    }
    pub async fn require_eligible(
        &self,
        tenant: Uuid,
        user: Uuid,
        issuer: &kabipay_organization_security::ServiceCredentialIssuer,
    ) -> KabiPayResult<()> {
        let mut url = self
            .endpoints
            .get(&tenant)
            .ok_or(KabiPayError::Unauthorised)?
            .clone();
        url.set_path("/auth/internal/account-eligibility");
        let nonce = Uuid::new_v4();
        let body = serde_json::to_vec(&kabipay_organization_protocol::AccountEligibilityRequest {
            user_id: user,
            nonce,
        })
        .map_err(|_| KabiPayError::Unauthorised)?;
        let token = issuer
            .issue(
                "kabipay-auth",
                tenant,
                "/auth/internal/account-eligibility",
                &body,
            )
            .map_err(|_| KabiPayError::Unauthorised)?;
        let mut response = self
            .http
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await
            .map_err(|_| identity_unavailable())?;
        eligibility_status(response.status())?;
        let mut body = Vec::new();
        while let Some(c) = response.chunk().await.map_err(|_| identity_unavailable())? {
            if body.len() + c.len() > 4096 {
                return Err(KabiPayError::Unauthorised);
            }
            body.extend_from_slice(&c);
        }
        let result: kabipay_organization_protocol::AccountEligibilityResponse =
            serde_json::from_slice(&body).map_err(|_| KabiPayError::Unauthorised)?;
        if result.tenant_id != tenant
            || result.user_id != user
            || result.nonce != nonce
            || !result.eligible
        {
            return Err(KabiPayError::Unauthorised);
        }
        Ok(())
    }
    pub fn new(endpoints: HashMap<Uuid, String>) -> KabiPayResult<Self> {
        let invalid = || {
            KabiPayError::Internal("invalid organization identity endpoint configuration".into())
        };
        if endpoints.is_empty() {
            return Err(invalid());
        }
        let mut parsed = HashMap::new();
        for (tenant, value) in endpoints {
            let url = reqwest::Url::parse(&value).map_err(|_| invalid())?;
            let loopback = url.host_str().is_some_and(|host| {
                host.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            });
            if tenant.is_nil()
                || !(url.scheme() == "https" || (url.scheme() == "http" && loopback))
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.path() != "/auth/client/organization-identity"
                || url.query().is_some()
                || url.fragment().is_some()
            {
                return Err(invalid());
            }
            parsed.insert(tenant, url);
        }
        let http = kabipay_common::organization_http::client(std::time::Duration::from_secs(5))
            .map_err(|_| invalid())?;
        Ok(Self {
            endpoints: parsed,
            http,
        })
    }
    pub async fn verify(&self, tenant: Uuid, bearer: &str) -> KabiPayResult<SourceIdentity> {
        let url = self
            .endpoints
            .get(&tenant)
            .ok_or(KabiPayError::Unauthorised)?;
        if !bearer.starts_with("Bearer ") || bearer.len() > 16384 {
            return Err(KabiPayError::Unauthorised);
        }
        let unavailable = || KabiPayError::Internal("source identity authority unavailable".into());
        let nonce = Uuid::new_v4();
        let mut response = self
            .http
            .post(url.clone())
            .header(reqwest::header::AUTHORIZATION, bearer)
            .json(&serde_json::json!({"nonce":nonce}))
            .send()
            .await
            .map_err(|_| unavailable())?;
        if !response.status().is_success() {
            return Err(KabiPayError::Unauthorised);
        }
        let mut body = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| unavailable())? {
            if body.len() + chunk.len() > 65536 {
                return Err(unavailable());
            }
            body.extend_from_slice(&chunk);
        }
        let value: IdentityResponse = serde_json::from_slice(&body).map_err(|_| unavailable())?;
        if value.nonce != nonce
            || value.tenant_id != tenant
            || value.user_id.is_nil()
            || value.issuer != "kabipay-client"
            || value.display_name.trim().is_empty()
            || value.display_name.len() > 255
        {
            return Err(KabiPayError::Unauthorised);
        }
        Ok(SourceIdentity {
            tenant,
            user: value.user_id,
            name: value.display_name,
            scopes: value.permission_scopes,
            asymmetric: value.assurance.as_deref() == Some("rs256-company-bound"),
        })
    }
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityResponse {
    nonce: Uuid,
    tenant_id: Uuid,
    user_id: Uuid,
    issuer: String,
    display_name: String,
    permission_scopes: HashMap<String, String>,
    #[serde(default)]
    assurance: Option<String>,
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn eligibility_outage_retries_but_authority_denial_is_terminal() {
        for status in [
            reqwest::StatusCode::SERVICE_UNAVAILABLE,
            reqwest::StatusCode::TOO_MANY_REQUESTS,
        ] {
            assert!(matches!(
                eligibility_status(status),
                Err(KabiPayError::Internal(_))
            ));
        }
        for status in [
            reqwest::StatusCode::UNAUTHORIZED,
            reqwest::StatusCode::FORBIDDEN,
            reqwest::StatusCode::TEMPORARY_REDIRECT,
        ] {
            assert!(matches!(
                eligibility_status(status),
                Err(KabiPayError::Unauthorised)
            ));
        }
    }
    use axum::{routing::post, Json, Router};
    #[test]
    fn endpoint_configuration_rejects_untrusted_transport_and_credentials() {
        for url in [
            "http://auth.example/auth/client/organization-identity",
            "https://user:secret@auth.example/auth/client/organization-identity",
            "https://auth.example/other",
            "https://auth.example/auth/client/organization-identity?redirect=evil",
        ] {
            assert!(
                IdentityClient::new(HashMap::from([(Uuid::from_u128(1), url.into())])).is_err()
            );
        }
        assert!(IdentityClient::new(HashMap::new()).is_err());
    }
    #[tokio::test]
    async fn live_identity_response_must_match_nonce_company_and_current_eligibility() {
        let app = Router::new().route("/auth/client/organization-identity", post(|Json(body): Json<serde_json::Value>| async move {
            Json(serde_json::json!({"nonce":body["nonce"],"tenant_id":Uuid::from_u128(1),"user_id":Uuid::from_u128(2),"issuer":"kabipay-client","display_name":"Verified account","permission_scopes":{"role:manage":"ALL"}}))
        }));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = IdentityClient::new(HashMap::from([(
            Uuid::from_u128(1),
            format!("http://{address}/auth/client/organization-identity"),
        )]))
        .unwrap();
        let identity = client
            .verify(Uuid::from_u128(1), "Bearer synthetic")
            .await
            .unwrap();
        assert_eq!(identity.user(), Uuid::from_u128(2));
        assert!(identity.may_accept_company());
        assert!(client
            .verify(Uuid::from_u128(3), "Bearer synthetic")
            .await
            .is_err());
        server.abort();
    }

    #[tokio::test]
    async fn substitutions_denials_redirects_and_cached_successes_cannot_supply_identity() {
        use axum::{
            http::StatusCode,
            response::{IntoResponse, Redirect},
        };
        let calls = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let app=Router::new().route("/auth/client/organization-identity",post(move |headers:axum::http::HeaderMap,Json(body):Json<serde_json::Value>| {let calls=calls.clone();async move {
            let mode=headers["authorization"].to_str().unwrap();
            if mode=="Bearer denied" {return StatusCode::UNAUTHORIZED.into_response();}
            if mode=="Bearer redirect" {return Redirect::temporary("/auth/client/organization-identity").into_response();}
            if mode=="Bearer oversize" {return "x".repeat(65537).into_response();}
            if mode=="Bearer live" && calls.fetch_add(1,std::sync::atomic::Ordering::SeqCst)>0 {return StatusCode::FORBIDDEN.into_response();}
            let mut value=serde_json::json!({"nonce":body["nonce"],"tenant_id":Uuid::from_u128(1),"user_id":Uuid::from_u128(2),"issuer":"kabipay-client","display_name":"Account","permission_scopes":{}});
            match mode {
                "Bearer nonce"=>value["nonce"]=serde_json::json!(Uuid::new_v4()),
                "Bearer company"=>value["tenant_id"]=serde_json::json!(Uuid::from_u128(3)),
                "Bearer user"=>value["user_id"]=serde_json::json!(Uuid::nil()),
                "Bearer issuer"=>value["issuer"]=serde_json::json!("kabipay-ops"),
                _=>{},
            }
            Json(value).into_response()
        }}));
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        let client = IdentityClient::new(HashMap::from([(
            Uuid::from_u128(1),
            format!("http://{address}/auth/client/organization-identity"),
        )]))
        .unwrap();
        for mode in [
            "nonce", "company", "user", "issuer", "denied", "redirect", "oversize",
        ] {
            assert!(client
                .verify(Uuid::from_u128(1), &format!("Bearer {mode}"))
                .await
                .is_err());
        }
        assert!(client
            .verify(Uuid::from_u128(1), "Bearer live")
            .await
            .is_ok());
        assert!(client
            .verify(Uuid::from_u128(1), "Bearer live")
            .await
            .is_err());
        server.abort();
    }
}
