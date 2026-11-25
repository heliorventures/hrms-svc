//! Private mail endpoints authenticate before selecting any company pool.
use crate::email_transport::{
    consume_email_proof, require_persisted_email_intent, verify_eligibility_transport,
    verify_recipient_transport,
};
use async_graphql::{ObjectType, Schema, SubscriptionType};
use axum::{
    body::Bytes,
    extract::{DefaultBodyLimit, State},
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use kabipay_common::{
    db::{company_public_origin, resolve_required_tenant_db, TenantDbCache, TenantDbConfig},
    important_email::*,
    KabiPayError, KabiPayResult,
};
use kabipay_organization_security::{
    service_issuer_from_env, service_verifier_from_env, ServiceCredentialVerifier,
    VerifiedServiceCredential,
};
use sea_orm::{DatabaseConnection, TransactionTrait};
use std::{future::Future, sync::Arc};

pub trait EmailDomainAdapter: Clone + Send + Sync + 'static {
    fn domain(&self) -> &'static str;
    fn check_email_eligibility(
        &self,
        db: &DatabaseConnection,
        ops: &DatabaseConnection,
        request: &EmailEligibilityRequest,
    ) -> impl Future<Output = KabiPayResult<EmailEligibilityDecision>> + Send;
}
type HttpFailure = (StatusCode, &'static str);
fn unavailable() -> HttpFailure {
    (
        StatusCode::SERVICE_UNAVAILABLE,
        "Email authority unavailable",
    )
}
fn unauthorized() -> HttpFailure {
    (StatusCode::UNAUTHORIZED, "Email authority unavailable")
}
fn bearer(headers: &HeaderMap) -> KabiPayResult<&str> {
    headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(KabiPayError::Unauthorised)
}

pub fn email_routes<Q, M, S, A>(adapter: A) -> Router<Arc<Schema<Q, M, S>>>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
    A: EmailDomainAdapter,
{
    build_routes(adapter, None)
}

/// Dependency injection for owned HTTP rehearsals and explicit host wiring.
/// Production's ordinary constructor retains the protected environment trust.
pub fn email_routes_with_verifier<Q, M, S, A>(
    adapter: A,
    verifier: Arc<ServiceCredentialVerifier>,
) -> Router<Arc<Schema<Q, M, S>>>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
    A: EmailDomainAdapter,
{
    build_routes(adapter, Some(verifier))
}
fn build_routes<Q, M, S, A>(
    adapter: A,
    verifier: Option<Arc<ServiceCredentialVerifier>>,
) -> Router<Arc<Schema<Q, M, S>>>
where
    Q: ObjectType + 'static,
    M: ObjectType + 'static,
    S: SubscriptionType + 'static,
    A: EmailDomainAdapter,
{
    Router::new()
        .route(
            EMAIL_ELIGIBILITY_ROUTE,
            post(
                move |State(schema): State<Arc<Schema<Q, M, S>>>,
                      headers: HeaderMap,
                      body: Bytes| {
                    let adapter = adapter.clone();
                    let verifier = verifier.clone();
                    async move {
                        // Parse and verify before touching schema database/cache data.
                        let (request, proof) = authenticate_eligibility(
                            adapter.domain(),
                            &headers,
                            &body,
                            verifier.as_deref(),
                        )
                        .map_err(|_| unauthorized())?;
                        let cache = schema.data::<TenantDbCache>().ok_or_else(unavailable)?;
                        let ops = schema
                            .data::<DatabaseConnection>()
                            .ok_or_else(unavailable)?;
                        let fallback = schema.data::<TenantDbConfig>().ok_or_else(unavailable)?;
                        let response =
                            eligibility(adapter, cache, ops, fallback, &request, &proof, &body)
                                .await
                                .map_err(|_| unavailable())?;
                        Ok::<_, HttpFailure>(Json(response))
                    }
                },
            ),
        )
        .layer(DefaultBodyLimit::max(EMAIL_PRIVATE_BODY_LIMIT))
}

fn authenticate_eligibility(
    domain: &str,
    headers: &HeaderMap,
    body: &[u8],
    supplied_verifier: Option<&ServiceCredentialVerifier>,
) -> KabiPayResult<(EmailEligibilityRequest, VerifiedServiceCredential)> {
    if body.len() > EMAIL_PRIVATE_BODY_LIMIT {
        return Err(KabiPayError::Unauthorised);
    }
    let request: EmailEligibilityRequest =
        serde_json::from_slice(body).map_err(|_| KabiPayError::Unauthorised)?;
    let proof = match supplied_verifier {
        Some(verifier) => {
            verify_eligibility_transport(verifier, bearer(headers)?, &request, body, domain)?
        }
        None => verify_eligibility_transport(
            &service_verifier_from_env(EMAIL_ELIGIBILITY_AUDIENCE)
                .map_err(|_| KabiPayError::Unauthorised)?,
            bearer(headers)?,
            &request,
            body,
            domain,
        )?,
    };
    Ok((request, proof))
}

async fn eligibility<A: EmailDomainAdapter>(
    adapter: A,
    cache: &TenantDbCache,
    ops: &DatabaseConnection,
    fallback: &TenantDbConfig,
    request: &EmailEligibilityRequest,
    proof: &VerifiedServiceCredential,
    body: &[u8],
) -> KabiPayResult<EmailEligibility> {
    let tenant = request.event.tenant_id;
    cache.require_tenant(tenant)?;
    let db = resolve_required_tenant_db(tenant, ops, cache, fallback).await?;
    kabipay_common::entitlements::Entitlements::load(ops, tenant)
        .await?
        .require(request.event.required_module())?;
    let tx = db.begin().await?;
    require_persisted_email_intent(&tx, request).await?;
    consume_email_proof(&tx, proof, tenant).await?;
    tx.commit().await?;
    // The nonce is consumed even when the resource has since become obsolete.
    let decision = adapter.check_email_eligibility(&db, ops, request).await?;
    request.validate()?;
    Ok(match decision {
        EmailEligibilityDecision::Obsolete => EmailEligibility::Obsolete,
        EmailEligibilityDecision::Forbidden => EmailEligibility::Forbidden,
        EmailEligibilityDecision::Eligible => EmailEligibility::Eligible {
            recipient_proof: service_issuer_from_env()
                .map_err(|_| KabiPayError::Unauthorised)?
                .issue(
                    EMAIL_RECIPIENT_AUDIENCE,
                    tenant,
                    EMAIL_RECIPIENT_ROUTE,
                    body,
                )
                .map_err(|_| KabiPayError::Unauthorised)?,
        },
    })
}

pub async fn domain_eligibility<A: EmailDomainAdapter>(
    adapter: A,
    cache: &TenantDbCache,
    ops: &DatabaseConnection,
    fallback: &TenantDbConfig,
    headers: &HeaderMap,
    body: &[u8],
) -> KabiPayResult<EmailEligibility> {
    let (request, proof) = authenticate_eligibility(adapter.domain(), headers, body, None)?;
    eligibility(adapter, cache, ops, fallback, &request, &proof, body).await
}

/// Employee's custom HTTP state uses this handler. The signed owning company
/// stays A while the verified recipient reference selects allowlisted source B.
pub async fn recipient(
    cache: &TenantDbCache,
    ops: &DatabaseConnection,
    fallback: &TenantDbConfig,
    headers: &HeaderMap,
    body: &[u8],
) -> KabiPayResult<EmailRecipientResolution> {
    if body.len() > EMAIL_PRIVATE_BODY_LIMIT {
        return Err(KabiPayError::Unauthorised);
    }
    let request: EmailEligibilityRequest =
        serde_json::from_slice(body).map_err(|_| KabiPayError::Unauthorised)?;
    let verifier = service_verifier_from_env(EMAIL_RECIPIENT_AUDIENCE)
        .map_err(|_| KabiPayError::Unauthorised)?;
    let proof = verify_recipient_transport(&verifier, bearer(headers)?, &request, body)?;
    let source_tenant = proof.source_tenant_id();
    cache.require_tenant(source_tenant)?;
    let origin = company_public_origin(source_tenant)?
        .ok_or_else(|| KabiPayError::Internal("Email source login origin unavailable".into()))?;
    let db = resolve_required_tenant_db(source_tenant, ops, cache, fallback).await?;
    let tx = db.begin().await?;
    consume_email_proof(&tx, proof.credential(), source_tenant).await?;
    let result = crate::email_identity::resolve_email_recipient(&tx, ops, &proof, &origin).await;
    // Denied identity probes consume their one-shot proof too. Database failures
    // return no private body and abort; retry still obtains a new bounded proof.
    if result.as_ref().is_ok() || matches!(&result, Err(KabiPayError::Unauthorised)) {
        tx.commit().await?;
    } else {
        tx.rollback().await?;
    }
    recipient_response(result)
}
fn recipient_response(result:KabiPayResult<EmailRecipientResolution>)->KabiPayResult<EmailRecipientResolution>{
 match result{Err(KabiPayError::Unauthorised)=>Ok(EmailRecipientResolution::Forbidden),other=>other}
}
#[cfg(test)]
mod recipient_response_tests{
 use super::*;
 #[test]
 fn authenticated_identity_revocation_is_distinct_from_transient_database_failure(){
  assert!(matches!(recipient_response(Err(KabiPayError::Unauthorised)),Ok(EmailRecipientResolution::Forbidden)));
  assert!(recipient_response(Err(KabiPayError::Internal("synthetic storage failure".into()))).is_err());
 }
}
