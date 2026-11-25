//! Mail-only live organization policy. Reuses the business authority selector;
//! it cannot execute a business command or resolve an email address.
use crate::{
    execution::Execution,
    runtime_policy,
    store::{denied, one, sql},
};
use axum::{body::Bytes, extract::State, http::HeaderMap, Json};
use kabipay_common::{important_email::*, KabiPayError, KabiPayResult};
use kabipay_organization_security::{AuthorityKind, ServiceCredentialVerifier};
use sea_orm::{ConnectionTrait, TransactionTrait};
use uuid::Uuid;

pub fn verify_email_policy(
    verifier: &ServiceCredentialVerifier,
    token: &str,
    body: &[u8],
) -> KabiPayResult<EmailOrganizationPolicyRequest> {
    if body.len() > EMAIL_PRIVATE_BODY_LIMIT {
        return Err(KabiPayError::Unauthorised);
    }
    let request: EmailOrganizationPolicyRequest =
        serde_json::from_slice(body).map_err(|_| KabiPayError::Unauthorised)?;
    request.validate()?;
    let proof = verifier
        .verify(
            token,
            request.intent.event.tenant_id,
            EMAIL_ORGANIZATION_POLICY_ROUTE,
            body,
        )
        .map_err(|_| KabiPayError::Unauthorised)?;
    if proof.claims().aud != EMAIL_ORGANIZATION_POLICY_AUDIENCE
        || proof.claims().service != format!("kabipay-{}", request.intent.event.producer())
    {
        return Err(KabiPayError::Unauthorised);
    }
    Ok(request)
}

pub async fn policy(
    State(engine): State<Execution>,
    headers: HeaderMap,
    body: Bytes,
) -> KabiPayResult<Json<EmailOrganizationPolicy>> {
    let token = headers
        .get("authorization")
        .and_then(|v| v.to_str().ok())
        .and_then(|v| v.strip_prefix("Bearer "))
        .ok_or(KabiPayError::Unauthorised)?;
    let verifier = kabipay_organization_security::service_verifier_from_env(
        EMAIL_ORGANIZATION_POLICY_AUDIENCE,
    )
    .map_err(|_| denied())?;
    let request = verify_email_policy(&verifier, token, &body)?;
    Ok(Json(check(&engine, &request).await?))
}

async fn check(
    engine: &Execution,
    request: &EmailOrganizationPolicyRequest,
) -> KabiPayResult<EmailOrganizationPolicy> {
    let event = &request.intent.event;
    let EmailRecipientRef::OrganizationPrincipal { principal_id, .. } = event.recipient else {
        return Err(denied());
    };
    let (org, member, authority_id, expected_version, expected_kind) = match event.authorization_ref
    {
        EmailAuthorizationRef::OrganizationGrant {
            organization_id,
            membership_id,
            binding_id,
            version,
        } => (
            organization_id,
            membership_id,
            binding_id,
            version,
            AuthorityKind::OrganizationGrant,
        ),
        EmailAuthorizationRef::OrganizationRelationship {
            organization_id,
            membership_id,
            relationship_id,
            version,
        } => (
            organization_id,
            membership_id,
            relationship_id,
            version,
            AuthorityKind::ManagementAssignment,
        ),
        _ => return Err(denied()),
    };
    let capability = email_action_permission(event.resource_type).ok_or_else(denied)?;
    let tx = engine.store.db.begin().await?;
    current_email_organization_user(&tx, event).await?;
    let (tenant, membership_version) = runtime_policy::membership(&tx, org, member).await?;
    if tenant != event.tenant_id {
        return Err(denied());
    }
    // A second employment under another company must not enable self-approval.
    let subject = one(&tx,"SELECT principal_id FROM kabipay_ops.employment_ref WHERE organization_id=$1 AND organization_company_id=$2 AND source_employee_id=$3 AND employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![org.into(),member.into(),request.subject_employee_id.into()]).await?;
    if subject.try_get::<Option<Uuid>>("", "principal_id")? == Some(principal_id) {
        return Err(denied());
    }
    let authority = runtime_policy::resolve_authority_matching(
        &tx,
        principal_id,
        org,
        member,
        capability,
        Some(request.subject_employee_id),
        false,
        Some(authority_id),
    )
    .await?;
    if authority.id != authority_id
        || authority.version != expected_version
        || authority.kind != expected_kind
    {
        return Err(denied());
    }
    let manager = tx.query_one(sql("SELECT a.id FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref subject ON subject.id=a.subject_employment_ref_id AND subject.organization_id=a.organization_id JOIN kabipay_ops.employment_ref manager ON manager.id=a.manager_employment_ref_id AND manager.organization_id=a.organization_id WHERE a.organization_id=$1 AND a.organization_company_id=$2 AND subject.source_employee_id=$3 AND manager.principal_id=$4 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.installed_version=a.version AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND a.capabilities @> $5::jsonb AND subject.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND manager.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') LIMIT 1",vec![org.into(),member.into(),request.subject_employee_id.into(),principal_id.into(),serde_json::json!([capability]).into()])).await?.is_some();
    request.validate()?;
    tx.commit().await?;
    Ok(EmailOrganizationPolicy {
        membership_version,
        scope: authority.scope,
        reporting_manager: manager,
    })
}
