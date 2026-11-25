//! Mail-only service proofs. These confer no organization-command authority.
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use kabipay_organization_security::{ServiceCredentialVerifier, VerifiedServiceCredential};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};

fn denied() -> KabiPayError {
    KabiPayError::Unauthorised
}

pub struct VerifiedEmailRecipient {
    pub(crate) credential: VerifiedServiceCredential,
    pub(crate) request: EmailEligibilityRequest,
}
impl VerifiedEmailRecipient {
    pub fn request(&self) -> &EmailEligibilityRequest {
        &self.request
    }
    pub fn credential(&self) -> &VerifiedServiceCredential {
        &self.credential
    }
    pub fn source_tenant_id(&self) -> uuid::Uuid {
        match self.request.event.recipient {
            EmailRecipientRef::LocalUser { .. } => self.request.event.tenant_id,
            EmailRecipientRef::OrganizationPrincipal {
                source_tenant_id, ..
            } => source_tenant_id,
        }
    }
}

pub fn verify_eligibility_transport(
    verifier: &ServiceCredentialVerifier,
    token: &str,
    request: &EmailEligibilityRequest,
    body: &[u8],
    receiving_domain: &str,
) -> KabiPayResult<VerifiedServiceCredential> {
    request.validate()?;
    if request.event.producer() != receiving_domain
        || body.len() > EMAIL_PRIVATE_BODY_LIMIT
        || serde_json::from_slice::<EmailEligibilityRequest>(body)
            .ok()
            .as_ref()
            != Some(request)
    {
        return Err(denied());
    }
    let proof = verifier
        .verify(
            token,
            request.event.tenant_id,
            EMAIL_ELIGIBILITY_ROUTE,
            body,
        )
        .map_err(|_| denied())?;
    if proof.claims().aud != EMAIL_ELIGIBILITY_AUDIENCE
        || proof.claims().service != "kabipay-notification"
    {
        return Err(denied());
    }
    Ok(proof)
}

pub fn verify_recipient_transport(
    verifier: &ServiceCredentialVerifier,
    token: &str,
    request: &EmailEligibilityRequest,
    body: &[u8],
) -> KabiPayResult<VerifiedEmailRecipient> {
    request.validate()?;
    if body.len() > EMAIL_PRIVATE_BODY_LIMIT
        || serde_json::from_slice::<EmailEligibilityRequest>(body)
            .ok()
            .as_ref()
            != Some(request)
    {
        return Err(denied());
    }
    // The key remains bound to the OWNING company. The exact signed body, never
    // an unsigned routing header, identifies a foreign recipient employment.
    let proof = verifier
        .verify(token, request.event.tenant_id, EMAIL_RECIPIENT_ROUTE, body)
        .map_err(|_| denied())?;
    if proof.claims().aud != EMAIL_RECIPIENT_AUDIENCE
        || proof.claims().service != format!("kabipay-{}", request.event.producer())
    {
        return Err(denied());
    }
    Ok(VerifiedEmailRecipient {
        credential: proof,
        request: request.clone(),
    })
}

/// Persist before performing a private lookup. A duplicate nonce is rejected,
/// including when the previous network response was lost. A retry needs a new proof.
pub async fn consume_email_proof(
    tx: &DatabaseTransaction,
    proof: &VerifiedServiceCredential,
    recipient_company: uuid::Uuid,
) -> KabiPayResult<()> {
    let claims = proof.claims();
    let purpose = match (claims.aud.as_str(), claims.route.as_str()) {
        (EMAIL_ELIGIBILITY_AUDIENCE, EMAIL_ELIGIBILITY_ROUTE) => "email_eligibility",
        (EMAIL_RECIPIENT_AUDIENCE, EMAIL_RECIPIENT_ROUTE) => "email_recipient",
        _ => return Err(denied()),
    };
    let expiry =
        chrono::DateTime::from_timestamp(i64::try_from(claims.exp).map_err(|_| denied())?, 0)
            .ok_or_else(denied)?;
    // Receipt consumers already own expired-only DELETE authority. Keep this
    // tenant-bounded and small; DELETE acquires its own row locks, so no UPDATE
    // grant is needed for a SELECT ... FOR UPDATE cleanup query.
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "DELETE FROM email_private_request_receipt WHERE tenant_id=$1 AND (audience,purpose,nonce) IN (SELECT audience,purpose,nonce FROM email_private_request_receipt WHERE tenant_id=$1 AND expires_at<clock_timestamp() ORDER BY expires_at,nonce LIMIT 100)",
        [recipient_company.into()]
    )).await?;
    let changed = tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO email_private_request_receipt (tenant_id,purpose,audience,nonce,request_hash,expires_at) VALUES($1,$2,$3,$4,$5,$6) ON CONFLICT DO NOTHING",
        [recipient_company.into(), purpose.into(), claims.aud.clone().into(), claims.jti.into(), claims.request_hash.clone().into(), expiry.into()]
    )).await?;
    if changed.rows_affected() != 1 {
        return Err(denied());
    }
    Ok(())
}

/// Full event reads are intentionally unnecessary for a producer. The immutable
/// ACK plus the recomputed canonical digest proves every signed event field.
pub async fn require_persisted_email_intent(
    db: &impl ConnectionTrait,
    request: &EmailEligibilityRequest,
) -> KabiPayResult<()> {
    request.validate()?;
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT event_hash FROM important_email_intent WHERE tenant_id=$1 AND id=$2 AND event_id=$3 AND recipient_key=$4",
        [request.event.tenant_id.into(), request.intent_id.into(), request.event.event_id.into(), request.event.recipient.key().into()]
    )).await?.ok_or_else(denied)?;
    if row.try_get::<String>("", "event_hash")? != request.event_hash {
        return Err(denied());
    }
    Ok(())
}
