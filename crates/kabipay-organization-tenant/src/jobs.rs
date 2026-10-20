//! Fresh durable-job authority. Persisted wire values cannot create private proof.
use crate::{statement, unavailable, VerifiedRuntimeAuthorization};
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::{entitlements::Entitlements, KabiPayResult};
use kabipay_organization_domain::{Capability, ResourceVersion};
use kabipay_organization_protocol::*;
use kabipay_organization_security::{self as security, CommandExpectation};
use sea_orm::{ConnectionTrait, DatabaseConnection, DatabaseTransaction};
use uuid::Uuid;

fn same_original_authority(a: &RuntimeAuthorization, b: &RuntimeAuthorization) -> bool {
    a.organization_id == b.organization_id
        && a.membership_id == b.membership_id
        && a.tenant_id == b.tenant_id
        && a.principal_id == b.principal_id
        && a.source_tenant_id == b.source_tenant_id
        && a.source_user_id == b.source_user_id
        && a.identity_version == b.identity_version
        && a.source_link_version == b.source_link_version
        && a.membership_version == b.membership_version
        && a.authority_kind == b.authority_kind
        && a.authority_id == b.authority_id
        && a.authority_version == b.authority_version
        && a.capability == b.capability
        && a.employee_scope == b.employee_scope
        && a.employee_id == b.employee_id
        && a.scope_employee_id == b.scope_employee_id
        && b.actor.principal_id == a.principal_id
        && b.actor.identity_version == a.identity_version
}

pub async fn refresh_payroll_job_authority(
    tx: &DatabaseTransaction,
    ops: &DatabaseConnection,
    tenant: Uuid,
    body: &[u8],
    original: &RuntimeAuthorization,
) -> KabiPayResult<VerifiedRuntimeAuthorization> {
    let c = crate::parse_command(body)?;
    if !matches!(
        c.operation,
        OrganizationOperation::PayrollDraft | OrganizationOperation::PayrollFinalize
    ) || original.tenant_id != tenant
        || original.capability != "payroll:manage"
        || original.employee_scope != "ALL"
        || original.authority_kind != AuthorityKind::OrganizationGrant
        || original.employee_id.is_some()
        || original.scope_employee_id.is_some()
    {
        return Err(unavailable());
    }
    let nonce = Uuid::new_v4();
    let response: PayrollJobAuthorizationResponse = crate::transport::policy_post(
        tenant,
        "/internal/organization/payroll-jobs/authorize",
        &PayrollJobAuthorizationRequest {
            original_command_body_base64: STANDARD.encode(body),
            original_authorization: original.clone(),
            nonce,
        },
    )
    .await?;
    if response.nonce != nonce {
        return Err(unavailable());
    }
    let hash = security::command_body_hash(body);
    let capability = Capability::parse(c.operation.capability()).map_err(|_| unavailable())?;
    let version =
        ResourceVersion::try_from(c.expected_version as u64).map_err(|_| unavailable())?;
    let verifier = security::delegation_verifier_from_env(tenant, "kabipay-payroll")
        .map_err(|_| unavailable())?;
    let verified = verifier
        .verify(
            &response.delegation_token,
            &CommandExpectation {
                capability: &capability,
                resource_type: c.operation.resource_type(),
                resource_id: c.resource_id,
                expected_resource_version: version,
                command_id: c.command_id,
                request_hash: &hash,
            },
        )
        .map_err(|_| unavailable())?;
    // A new source-service-authenticated live lookup binds the signed claims to
    // current account/link/grant state; a JSON policy response alone is no proof.
    let policy: RuntimeAuthorization = crate::transport::policy_post(
        tenant,
        "/internal/organization/policy",
        &RuntimePolicyRequest {
            delegation_token: response.delegation_token,
            command_body_base64: STANDARD.encode(body),
            employee_id: None,
        },
    )
    .await?;
    if !crate::transport::matches_policy(verified.claims(), &policy, None)
        || !same_original_authority(original, &policy)
    {
        return Err(unavailable());
    }
    crate::membership_lifecycle::require_active(
        tx,
        tenant,
        policy.membership_id,
        policy.organization_id,
        policy.membership_version,
    )
    .await?;
    crate::transport::local_authority(tx, &policy, None).await?;
    crate::installation::require_actor(tx, tenant, &policy.actor).await?;
    let local_user_id = if policy.source_tenant_id == tenant {
        if tx.query_one(statement("SELECT id FROM \"user\" WHERE tenant_id=$1 AND id=$2 AND is_active AND NOT is_deleted FOR SHARE",vec![tenant.into(),policy.source_user_id.into()])).await?.is_none() {return Err(unavailable());}
        Some(policy.source_user_id)
    } else {
        None
    };
    let entitlements = Entitlements::load(ops, tenant).await?;
    entitlements.require_tenant(tenant)?;
    entitlements.require("PAYROLL")?;
    let actor = CompanyEventActor::OrganizationPrincipal {
        principal_id: policy.principal_id,
        display_name: policy.actor.display_name.clone(),
        identity_version: policy.identity_version,
        source_tenant_id: policy.source_tenant_id,
        source_user_id: policy.source_user_id,
        authority_kind: policy.authority_kind.clone(),
        authority_id: policy.authority_id,
        authority_version: policy.authority_version,
        command_id: c.command_id,
    };
    tx.execute(statement(
        "SELECT set_config('kabipay.organization_actor',$1,true)",
        vec![serde_json::to_string(&actor)?.into()],
    ))
    .await?;
    Ok(VerifiedRuntimeAuthorization {
        policy,
        operation: c.operation,
        command_id: c.command_id,
        entitlements: Some(entitlements),
        local_user_id,
        request_body: body.to_vec(),
        request_hash: hash,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persisted_authority_cannot_accept_replacement_grant_or_company() {
        let mut p = RuntimeAuthorization {
            organization_id: Uuid::new_v4(),
            membership_id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            principal_id: Uuid::new_v4(),
            source_tenant_id: Uuid::new_v4(),
            source_user_id: Uuid::new_v4(),
            identity_version: 1,
            source_link_version: 1,
            membership_version: 1,
            authority_kind: AuthorityKind::OrganizationGrant,
            authority_id: Uuid::new_v4(),
            authority_version: 1,
            capability: "payroll:manage".into(),
            employee_scope: "ALL".into(),
            employee_id: None,
            scope_employee_id: None,
            actor_target_user_ids: Vec::new(),
            actor: ActorReference {
                principal_id: Uuid::nil(),
                identity_version: 1,
                display_name: "Actor".into(),
            },
            grant: None,
            relationship: None,
            membership: None,
        };
        p.actor.principal_id = p.principal_id;
        assert!(same_original_authority(&p, &p));
        for i in 0..14 {
            let mut fresh = p.clone();
            match i {
                0 => fresh.authority_id = Uuid::new_v4(),
                1 => fresh.authority_version += 1,
                2 => fresh.source_link_version += 1,
                3 => fresh.identity_version += 1,
                4 => fresh.membership_version += 1,
                5 => fresh.tenant_id = Uuid::new_v4(),
                6 => fresh.source_tenant_id = Uuid::new_v4(),
                7 => fresh.source_user_id = Uuid::new_v4(),
                8 => {
                    fresh.principal_id = Uuid::new_v4();
                    fresh.actor.principal_id = fresh.principal_id;
                }
                9 => fresh.membership_id = Uuid::new_v4(),
                10 => fresh.organization_id = Uuid::new_v4(),
                11 => fresh.employee_scope = "SELF".into(),
                12 => fresh.authority_kind = AuthorityKind::ManagementAssignment,
                _ => fresh.actor.identity_version += 1,
            }
            assert!(!same_original_authority(&p, &fresh));
        }
    }
}
