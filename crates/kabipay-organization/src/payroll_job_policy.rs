//! Only the owning payroll runtime may refresh an immutable accepted job intent.
use crate::{
    execution::{op_code, valid_command, version, Execution},
    identity::SourceIdentity,
    policy_endpoint::service_proof,
    runtime_policy::*,
    store::*,
};
use axum::{body::Bytes, extract::State, http::HeaderMap, Json};
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::KabiPayResult;
use kabipay_organization_domain::Capability;
use kabipay_organization_protocol::*;
use kabipay_organization_security::{command_body_hash, DelegationClaims, DELEGATION_ISSUER};
use sea_orm::TransactionTrait;
use serde::Deserialize;
use uuid::Uuid;

#[derive(Deserialize)]
struct JobIntent {
    organization_id: Uuid,
    membership_id: Uuid,
    tenant_id: Uuid,
    principal_id: Uuid,
    source_tenant_id: Uuid,
    source_user_id: Uuid,
    operation: String,
    resource_id: Uuid,
    expected_version: i64,
    request_hash: String,
    authority_kind: String,
    authority_id: Uuid,
    authority_version: i64,
    membership_version: i64,
    identity_version: Option<i64>,
    source_link_version: Option<i64>,
    status: String,
}
impl JobIntent {
    fn matches(
        &self,
        tenant: Uuid,
        c: &OrganizationCommand,
        hash: &str,
        p: &RuntimeAuthorization,
    ) -> bool {
        matches!(
            c.operation,
            OrganizationOperation::PayrollDraft | OrganizationOperation::PayrollFinalize
        ) && p.authority_kind == AuthorityKind::OrganizationGrant
            && p.employee_scope == "ALL"
            && p.capability == "payroll:manage"
            && p.employee_id.is_none()
            && p.scope_employee_id.is_none()
            && self.tenant_id == tenant
            && p.tenant_id == tenant
            && self.organization_id == p.organization_id
            && self.membership_id == p.membership_id
            && self.principal_id == p.principal_id
            && self.source_tenant_id == p.source_tenant_id
            && self.source_user_id == p.source_user_id
            && self.resource_id == c.resource_id
            && self.expected_version == c.expected_version
            && self.request_hash == hash
            && op_code(c.operation).is_ok_and(|op| op == self.operation)
            && self.authority_kind == "organization_grant"
            && self.authority_id == p.authority_id
            && self.authority_version == p.authority_version
            && self.membership_version == p.membership_version
            && self.identity_version == Some(p.identity_version)
            && self.source_link_version == Some(p.source_link_version)
            && matches!(
                self.status.as_str(),
                "SUBMITTED" | "AWAITING_CONFIRMATION" | "ACCEPTED"
            )
    }
}

pub async fn authorize(
    State(engine): State<Execution>,
    headers: HeaderMap,
    body: Bytes,
) -> KabiPayResult<Json<PayrollJobAuthorizationResponse>> {
    let (tenant, service) = service_proof(
        &engine,
        &headers,
        "/internal/organization/payroll-jobs/authorize",
        &body,
    )?;
    if service != "kabipay-payroll" {
        return Err(denied());
    }
    let request: PayrollJobAuthorizationRequest = serde_json::from_slice(&body)?;
    if request.nonce.is_nil() || request.original_command_body_base64.len() > 90000 {
        return Err(denied());
    }
    let raw = STANDARD
        .decode(&request.original_command_body_base64)
        .map_err(|_| denied())?;
    if raw.len() > 65536 {
        return Err(denied());
    }
    let command: OrganizationCommand = serde_json::from_slice(&raw)?;
    valid_command(&command)?;
    let reference = &request.original_authorization;
    let hash = command_body_hash(&raw);
    engine
        .identities
        .require_eligible(
            reference.source_tenant_id,
            reference.source_user_id,
            &engine.service_issuer,
        )
        .await?;
    let actor = SourceIdentity::eligible_source(
        reference.source_tenant_id,
        reference.source_user_id,
        String::new(),
    );
    let tx = engine.store.db.begin().await?;
    let row=one(&tx,"SELECT to_jsonb(i) AS data FROM kabipay_ops.organization_command_intent i WHERE command_id=$1 FOR SHARE",vec![command.command_id.into()]).await?;
    let intent: JobIntent = serde_json::from_value(row.try_get("", "data")?)?;
    if !intent.matches(tenant, &command, &hash, reference) {
        return Err(denied());
    }
    let p = current(&tx, &actor).await?;
    let (target, mv) = membership(&tx, reference.organization_id, reference.membership_id).await?;
    if target != tenant
        || p.id != reference.principal_id
        || p.identity_version != reference.identity_version
        || p.link_version != reference.source_link_version
        || mv != reference.membership_version
    {
        return Err(denied());
    }
    let auth = authority(
        &tx,
        &actor,
        p.id,
        reference.organization_id,
        reference.membership_id,
        command.operation,
        None,
    )
    .await?;
    if auth.kind != reference.authority_kind
        || auth.id != reference.authority_id
        || auth.version != reference.authority_version
        || auth.scope != "ALL"
    {
        return Err(denied());
    }
    let now = u64::try_from(chrono::Utc::now().timestamp()).map_err(|_| denied())?;
    let claims = DelegationClaims {
        iss: DELEGATION_ISSUER.into(),
        aud: "kabipay-payroll".into(),
        sub: p.id.try_into().map_err(|_| denied())?,
        jti: Uuid::new_v4(),
        iat: now,
        nbf: now,
        exp: now + 120,
        organization_id: reference.organization_id.try_into().map_err(|_| denied())?,
        target_membership_id: reference.membership_id.try_into().map_err(|_| denied())?,
        source_tenant_id: reference
            .source_tenant_id
            .try_into()
            .map_err(|_| denied())?,
        source_user_id: reference.source_user_id,
        target_tenant_id: tenant.try_into().map_err(|_| denied())?,
        identity_version: version(p.identity_version)?,
        source_link_version: version(p.link_version)?,
        target_membership_version: version(mv)?,
        authority_kind: auth.kind,
        authority_ref: auth.id,
        authority_version: version(auth.version)?,
        capability: Capability::parse("payroll:manage").map_err(|_| denied())?,
        resource_type: command.operation.resource_type().into(),
        resource_id: command.resource_id,
        expected_resource_version: version(command.expected_version)?,
        command_id: command.command_id,
        request_hash: hash,
    };
    let token = engine.issuer.issue(&claims).map_err(|_| denied())?;
    tx.commit().await?;
    Ok(Json(PayrollJobAuthorizationResponse {
        delegation_token: token,
        nonce: request.nonce,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn jobs_cannot_refresh_unknown_epochs_replaced_grants_or_another_source_actor() {
        let p = RuntimeAuthorization {
            organization_id: Uuid::new_v4(),
            membership_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            principal_id: Uuid::new_v4(),
            source_tenant_id: Uuid::new_v4(),
            source_user_id: Uuid::new_v4(),
            identity_version: 2,
            source_link_version: 3,
            membership_version: 4,
            authority_kind: AuthorityKind::OrganizationGrant,
            authority_id: Uuid::new_v4(),
            authority_version: 5,
            capability: "payroll:manage".into(),
            employee_scope: "ALL".into(),
            employee_id: None,
            scope_employee_id: None,
            actor_target_user_ids: Vec::new(),
            actor: ActorReference {
                principal_id: Uuid::new_v4(),
                display_name: String::new(),
                identity_version: 2,
            },
            grant: None,
            relationship: None,
            membership: None,
        };
        let c = OrganizationCommand {
            command_id: Uuid::new_v4(),
            resource_id: Uuid::new_v4(),
            expected_version: 6,
            operation: OrganizationOperation::PayrollDraft,
            payload: serde_json::json!({}),
        };
        let mut i = JobIntent {
            organization_id: p.organization_id,
            membership_id: p.membership_id,
            tenant_id: p.tenant_id,
            principal_id: p.principal_id,
            source_tenant_id: p.source_tenant_id,
            source_user_id: p.source_user_id,
            operation: "payroll_draft".into(),
            resource_id: c.resource_id,
            expected_version: 6,
            request_hash: "exact".into(),
            authority_kind: "organization_grant".into(),
            authority_id: p.authority_id,
            authority_version: 5,
            membership_version: 4,
            identity_version: Some(2),
            source_link_version: Some(3),
            status: "ACCEPTED".into(),
        };
        assert!(i.matches(p.tenant_id, &c, "exact", &p));
        i.identity_version = None;
        assert!(!i.matches(p.tenant_id, &c, "exact", &p));
        i.identity_version = Some(2);
        i.authority_version = 6;
        assert!(!i.matches(p.tenant_id, &c, "exact", &p));
        i.authority_version = 5;
        assert!(!i.matches(Uuid::new_v4(), &c, "exact", &p));
        assert!(!i.matches(p.tenant_id, &c, "changed", &p));
        i.status = "CANCELLED_NOT_EXECUTED".into();
        assert!(!i.matches(p.tenant_id, &c, "exact", &p));
    }
}
