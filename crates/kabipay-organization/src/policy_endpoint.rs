use crate::policy::{DELEGATE, MANAGE};
use crate::{
    execution::{op_code, valid_command, version, Execution},
    identity::SourceIdentity,
    runtime_policy::*,
    store::*,
};
use axum::{
    body::Bytes,
    extract::State,
    http::{header, HeaderMap},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::{jwt::extract_bearer, KabiPayError, KabiPayResult};
use kabipay_organization_domain::Capability;
use kabipay_organization_protocol::*;
use kabipay_organization_security::{
    command_body_hash, delegation_verifier_from_env, CommandExpectation,
};
use sea_orm::{ConnectionTrait, TransactionTrait};
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
pub(crate) fn service_proof(
    engine: &Execution,
    headers: &HeaderMap,
    route: &str,
    body: &[u8],
) -> KabiPayResult<(Uuid, String)> {
    let tenant = headers
        .get("x-organization-company-id")
        .and_then(|s| s.to_str().ok())
        .and_then(|s| Uuid::parse_str(s).ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let bearer = headers
        .get(header::AUTHORIZATION)
        .and_then(|s| s.to_str().ok())
        .ok_or(KabiPayError::Unauthorised)?;
    let proof = engine
        .service_verifier
        .verify(extract_bearer(bearer)?, tenant, route, body)
        .map_err(|_| KabiPayError::Unauthorised)?;
    Ok((tenant, proof.claims().service.clone()))
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptQuery {
    original_command_id: Uuid,
    original_operation: OrganizationOperation,
    request_hash: String,
    original_resource_id: Option<Uuid>,
}
pub async fn policy(
    State(engine): State<Execution>,
    headers: HeaderMap,
    body: Bytes,
) -> KabiPayResult<Json<RuntimeAuthorization>> {
    let (tenant, service) =
        service_proof(&engine, &headers, "/internal/organization/policy", &body)?;
    let request: RuntimePolicyRequest = serde_json::from_slice(&body)?;
    let raw = STANDARD
        .decode(&request.command_body_base64)
        .map_err(|_| KabiPayError::Unauthorised)?;
    let command: OrganizationCommand = serde_json::from_slice(&raw)?;
    valid_command(&command)?;
    let receipt = if matches!(
        command.operation,
        OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
    ) {
        Some(serde_json::from_value::<ReceiptQuery>(
            command.payload.clone(),
        )?)
    } else {
        None
    };
    let operation = receipt
        .as_ref()
        .map(|r| r.original_operation)
        .unwrap_or(command.operation);
    if matches!(
        operation,
        OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
    ) || operation.service() != service
    {
        return Err(denied());
    }
    let cap = Capability::parse(operation.capability()).map_err(|_| denied())?;
    let hash = command_body_hash(&raw);
    let verified = delegation_verifier_from_env(tenant, operation.service())
        .map_err(|_| denied())?
        .verify(
            &request.delegation_token,
            &CommandExpectation {
                capability: &cap,
                resource_type: command.operation.resource_type(),
                resource_id: command.resource_id,
                expected_resource_version: version(command.expected_version)?,
                command_id: command.command_id,
                request_hash: &hash,
            },
        )
        .map_err(|_| KabiPayError::Unauthorised)?;
    let c = verified.claims();
    let source: Uuid = c.source_tenant_id.into();
    engine
        .identities
        .require_eligible(source, c.source_user_id, &engine.service_issuer)
        .await?;
    let actor = SourceIdentity::eligible_source(source, c.source_user_id, String::new());
    let tx = engine.store.db.begin().await?;
    let p = current(&tx, &actor).await?;
    let org: Uuid = c.organization_id.into();
    let member: Uuid = c.target_membership_id.into();
    let (target, mv) = if operation == OrganizationOperation::InstallMembership {
        lifecycle_membership(&tx, org, member).await?
    } else {
        membership(&tx, org, member).await?
    };
    if target != tenant
        || p.id != Uuid::from(c.sub)
        || p.identity_version as u64 != u64::from(c.identity_version)
        || p.link_version as u64 != u64::from(c.source_link_version)
        || mv as u64 != u64::from(c.target_membership_version)
    {
        return Err(denied());
    }
    let mut original_resource = command.resource_id;
    if let Some(r) = &receipt {
        if r.original_command_id != command.resource_id {
            return Err(denied());
        }
        let row=one(&tx,"SELECT principal_id,source_tenant_id,source_user_id,organization_id,membership_id,resource_id,operation,request_hash,expected_version,employee_id FROM kabipay_ops.organization_command_intent WHERE command_id=$1 FOR UPDATE",vec![r.original_command_id.into()]).await?;
        if row.try_get::<Uuid>("", "principal_id")? != p.id
            || row.try_get::<Uuid>("", "source_tenant_id")? != source
            || row.try_get::<Uuid>("", "source_user_id")? != c.source_user_id
            || row.try_get::<Uuid>("", "organization_id")? != org
            || row.try_get::<Uuid>("", "membership_id")? != member
            || row.try_get::<String>("", "request_hash")? != r.request_hash
            || row.try_get::<String>("", "operation")? != op_code(operation)?
            || row.try_get::<i64>("", "expected_version")? != command.expected_version
            || (row.try_get::<Option<Uuid>>("", "employee_id")? != request.employee_id
                && !(command.operation == OrganizationOperation::CancelCommand
                    && row.try_get::<Option<Uuid>>("", "employee_id")?.is_none()))
        {
            return Err(denied());
        }
        if command.operation == OrganizationOperation::CancelCommand
            && row.try_get::<Option<Uuid>>("", "employee_id")?.is_none()
            && request.employee_id.is_some()
        {
            let changed=tx.execute(sql("UPDATE kabipay_ops.organization_command_intent SET employee_id=$2 WHERE command_id=$1 AND employee_id IS NULL",vec![r.original_command_id.into(),request.employee_id.into()])).await?;
            if changed.rows_affected() != 1 {
                return Err(conflict());
            }
        }
        original_resource = row.try_get("", "resource_id")?;
        if command.operation == OrganizationOperation::CancelCommand
            && r.original_resource_id != Some(original_resource)
        {
            return Err(denied());
        }
    } else if !command.operation.is_read() {
        let row=one(&tx,"SELECT principal_id,source_tenant_id,source_user_id,organization_id,membership_id,request_hash,employee_id,authority_id,authority_version FROM kabipay_ops.organization_command_intent WHERE command_id=$1 FOR UPDATE",vec![command.command_id.into()]).await?;
        let saved_employee: Option<Uuid> = row.try_get("", "employee_id")?;
        if row.try_get::<Uuid>("", "principal_id")? != p.id
            || row.try_get::<Uuid>("", "source_tenant_id")? != source
            || row.try_get::<Uuid>("", "source_user_id")? != c.source_user_id
            || row.try_get::<Uuid>("", "organization_id")? != org
            || row.try_get::<Uuid>("", "membership_id")? != member
            || row.try_get::<String>("", "request_hash")? != hash
            || row.try_get::<Uuid>("", "authority_id")? != c.authority_ref
            || row.try_get::<i64>("", "authority_version")? as u64 != u64::from(c.authority_version)
            || saved_employee.is_some() && saved_employee != request.employee_id
        {
            return Err(denied());
        }
        if saved_employee.is_none() && request.employee_id.is_some() {
            tx.execute(sql("UPDATE kabipay_ops.organization_command_intent SET employee_id=$2 WHERE command_id=$1 AND employee_id IS NULL",vec![command.command_id.into(),request.employee_id.into()])).await?;
        }
    }
    let auth = authority(
        &tx,
        &actor,
        p.id,
        org,
        member,
        operation,
        request.employee_id,
    )
    .await?;
    if auth.kind == AuthorityKind::ManagementAssignment {
        let manager=one(&tx,"SELECT m.source_tenant_id,m.source_employee_id,m.source_user_id FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref m ON m.id=a.manager_employment_ref_id AND m.organization_id=a.organization_id WHERE a.id=$1 AND a.organization_id=$2",vec![auth.id.into(),org.into()]).await?;
        engine
            .identities
            .require_employment_eligible(
                manager.try_get("", "source_tenant_id")?,
                manager.try_get("", "source_employee_id")?,
                manager.try_get("", "source_user_id")?,
                &engine.service_issuer,
            )
            .await?;
    }
    if auth.kind != c.authority_kind
        || auth.id != c.authority_ref
        || auth.version as u64 != u64::from(c.authority_version)
    {
        return Err(denied());
    }
    let grant = if operation == OrganizationOperation::InstallGrant {
        let g = grant_configuration(&tx, org, member, original_resource).await?;
        if g.grant_version != command.expected_version {
            return Err(conflict());
        }
        Some(g)
    } else {
        None
    };
    let relationship = if operation == OrganizationOperation::InstallRelationship {
        let g = relationship_configuration(&tx, org, member, original_resource).await?;
        if g.version != command.expected_version {
            return Err(conflict());
        }
        Some(g)
    } else {
        None
    };
    if operation == OrganizationOperation::Directory
        && (command.resource_id != member || command.expected_version != mv)
    {
        return Err(conflict());
    }
    let scope_employee_id = if matches!(auth.scope.as_str(), "SELF" | "DEPARTMENT") {
        let rows=tx.query_all(sql("SELECT source_employee_id FROM kabipay_ops.employment_ref WHERE organization_id=$1 AND organization_company_id=$2 AND principal_id=$3 AND employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') ORDER BY id LIMIT 2",vec![org.into(),member.into(),p.id.into()])).await?;
        if rows.len() != 1 {
            return Err(KabiPayError::Forbidden(
                "employee-scoped authority requires one verified target-company employment".into(),
            ));
        }
        Some(rows[0].try_get("", "source_employee_id")?)
    } else {
        None
    };
    let target_accounts=tx.query_all(sql("SELECT source_user_id FROM kabipay_ops.principal_login_link WHERE principal_id=$1 AND source_tenant_id=$2 AND issuer='kabipay-client' AND status='VERIFIED' ORDER BY source_user_id LIMIT 101",vec![p.id.into(),tenant.into()])).await?;
    if target_accounts.len() > 100 {
        return Err(denied());
    }
    let actor_target_user_ids = target_accounts
        .iter()
        .map(|r| r.try_get("", "source_user_id"))
        .collect::<Result<Vec<Uuid>, _>>()?;
    let result = RuntimeAuthorization {
        organization_id: org,
        membership_id: member,
        tenant_id: tenant,
        principal_id: p.id,
        source_tenant_id: source,
        source_user_id: c.source_user_id,
        identity_version: p.identity_version,
        source_link_version: p.link_version,
        membership_version: mv,
        authority_kind: auth.kind,
        authority_id: auth.id,
        authority_version: auth.version,
        capability: operation.capability().into(),
        employee_scope: auth.scope,
        employee_id: request.employee_id,
        scope_employee_id,
        actor_target_user_ids,
        actor: ActorReference {
            principal_id: p.id,
            display_name: p.name,
            identity_version: p.identity_version,
        },
        grant,
        relationship,
        membership: if operation == OrganizationOperation::InstallMembership {
            let g = crate::membership_execution::configuration(&tx, org, member).await?;
            if g.version != command.expected_version || original_resource != member {
                return Err(conflict());
            }
            Some(g)
        } else {
            None
        },
    };
    tx.commit().await?;
    Ok(Json(result))
}
async fn ack_actor(
    engine: &Execution,
    command: Uuid,
    tenant: Uuid,
    op: OrganizationOperation,
) -> KabiPayResult<(SourceIdentity, Uuid, Uuid)> {
    let row=one(&engine.store.db,"SELECT i.organization_id,i.membership_id,i.source_tenant_id,i.source_user_id,i.operation,i.tenant_id FROM kabipay_ops.organization_command_intent i JOIN kabipay_ops.principal_login_link l ON l.principal_id=i.principal_id AND l.source_tenant_id=i.source_tenant_id AND l.source_user_id=i.source_user_id AND l.issuer='kabipay-client' AND l.status='VERIFIED' WHERE i.command_id=$1",vec![command.into()]).await?;
    if row.try_get::<Uuid>("", "tenant_id")? != tenant
        || row.try_get::<String>("", "operation")? != op_code(op)?
    {
        return Err(denied());
    }
    let source = row.try_get("", "source_tenant_id")?;
    let user = row.try_get("", "source_user_id")?;
    engine
        .identities
        .require_eligible(source, user, &engine.service_issuer)
        .await?;
    Ok((
        SourceIdentity::eligible_source(source, user, String::new()),
        row.try_get("", "organization_id")?,
        row.try_get("", "membership_id")?,
    ))
}
pub async fn grant_ack(
    State(engine): State<Execution>,
    headers: HeaderMap,
    body: Bytes,
) -> KabiPayResult<Json<Value>> {
    let (tenant, service) = service_proof(
        &engine,
        &headers,
        "/internal/organization/grants/ack",
        &body,
    )?;
    if service != "kabipay-employee" {
        return Err(denied());
    }
    let ack: GrantAcknowledgement = serde_json::from_slice(&body)?;
    let (actor, org, member) = ack_actor(
        &engine,
        ack.command_id,
        tenant,
        OrganizationOperation::InstallGrant,
    )
    .await?;
    if member != ack.membership_id {
        return Err(denied());
    }
    let tx = engine.store.db.begin().await?;
    one(
        &tx,
        "SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",
        vec![org.into()],
    )
    .await?;
    let p = current(&tx, &actor).await?;
    central_authority(&tx, &actor, p.id, org, DELEGATE).await?;
    let g = grant_configuration(&tx, org, member, ack.binding_id).await?;
    let intent=one(&tx,"SELECT resource_id,expected_version FROM kabipay_ops.organization_command_intent WHERE command_id=$1",vec![ack.command_id.into()]).await?;
    if g.configuration_hash != ack.configuration_hash
        || g.grant_version != ack.grant_version
        || intent.try_get::<Uuid>("", "resource_id")? != ack.binding_id
        || intent.try_get::<i64>("", "expected_version")? != ack.grant_version
    {
        return Err(conflict());
    }
    let changed=tx.execute(sql("UPDATE kabipay_ops.role_binding_company SET activation_status='ACTIVE',installed_hash=$4 WHERE organization_id=$1 AND binding_id=$2 AND organization_company_id=$3 AND expected_grant_version=$5 AND activation_status IN ('PENDING','ACTIVE') AND (activation_status<>'ACTIVE' OR installed_hash IS DISTINCT FROM $4)",vec![org.into(),ack.binding_id.into(),member.into(),ack.configuration_hash.into(),ack.grant_version.into()])).await?;
    tx.execute(sql("UPDATE kabipay_ops.principal_role_binding SET status='ACTIVE',updated_at=NOW() WHERE organization_id=$1 AND id=$2 AND version=$3 AND status IN ('PENDING','ACTIVE')",vec![org.into(),ack.binding_id.into(),ack.grant_version.into()])).await?;
    if changed.rows_affected() == 1 {
        event(
            &tx,
            &actor,
            p.id,
            EventSubject {
                organization: Some(org),
                id: ack.binding_id,
                version: ack.grant_version,
            },
            "grant.installed",
            json!({"membership_id":member,"binding_id":ack.binding_id,"version":ack.grant_version}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"acknowledged":true})))
}
pub async fn relationship_ack(
    State(engine): State<Execution>,
    headers: HeaderMap,
    body: Bytes,
) -> KabiPayResult<Json<Value>> {
    let (tenant, service) = service_proof(
        &engine,
        &headers,
        "/internal/organization/relationships/ack",
        &body,
    )?;
    if service != "kabipay-employee" {
        return Err(denied());
    }
    let ack: RelationshipAcknowledgement = serde_json::from_slice(&body)?;
    let (actor, org, member) = ack_actor(
        &engine,
        ack.command_id,
        tenant,
        OrganizationOperation::InstallRelationship,
    )
    .await?;
    if member != ack.membership_id {
        return Err(denied());
    }
    let tx = engine.store.db.begin().await?;
    one(
        &tx,
        "SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",
        vec![org.into()],
    )
    .await?;
    let p = current(&tx, &actor).await?;
    central_authority(&tx, &actor, p.id, org, MANAGE).await?;
    let r = relationship_configuration(&tx, org, member, ack.assignment_id).await?;
    let intent=one(&tx,"SELECT resource_id,expected_version FROM kabipay_ops.organization_command_intent WHERE command_id=$1",vec![ack.command_id.into()]).await?;
    if r.configuration_hash != ack.configuration_hash
        || r.version != ack.version
        || r.graph_version != ack.graph_version
        || intent.try_get::<Uuid>("", "resource_id")? != ack.assignment_id
        || intent.try_get::<i64>("", "expected_version")? != ack.version
    {
        return Err(conflict());
    }
    let changed=tx.execute(sql("UPDATE kabipay_ops.organization_relationship SET status=CASE WHEN status='ENDED' THEN 'ENDED' ELSE 'ACTIVE' END,installed_version=$4,installed_hash=$5,installed_configuration=$6,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND organization_company_id=$3 AND version=$4 AND (installed_version IS DISTINCT FROM $4 OR installed_hash IS DISTINCT FROM $5)",vec![ack.assignment_id.into(),org.into(),member.into(),ack.version.into(),ack.configuration_hash.into(),serde_json::to_value(&r)?.into()])).await?;
    if changed.rows_affected() == 1 {
        event(&tx,&actor,p.id,EventSubject{organization:Some(org),id:ack.assignment_id,version:ack.version},"relationship.installed",json!({"id":ack.assignment_id,"membership_id":member,"version":ack.version,"graph_version":ack.graph_version})).await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"acknowledged":true})))
}

/// Only the bound Employee service can confirm its installed membership fence.
pub async fn membership_ack(
    State(engine): State<Execution>,
    headers: HeaderMap,
    body: Bytes,
) -> KabiPayResult<Json<Value>> {
    let (tenant, service) = service_proof(
        &engine,
        &headers,
        "/internal/organization/memberships/ack",
        &body,
    )?;
    if service != "kabipay-employee" {
        return Err(denied());
    }
    let ack: MembershipAcknowledgement = serde_json::from_slice(&body)?;
    let (actor, org, member) = ack_actor(
        &engine,
        ack.command_id,
        tenant,
        OrganizationOperation::InstallMembership,
    )
    .await?;
    if member != ack.membership_id {
        return Err(denied());
    }
    let tx = engine.store.db.begin().await?;
    one(
        &tx,
        "SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",
        vec![org.into()],
    )
    .await?;
    let p = current(&tx, &actor).await?;
    central_authority(&tx, &actor, p.id, org, MANAGE).await?;
    let g = crate::membership_execution::configuration(&tx, org, member).await?;
    let expected_status = if g.status == "DETACHING" {
        "DETACHED"
    } else {
        g.status.as_str()
    };
    let intent=one(&tx,"SELECT resource_id,expected_version FROM kabipay_ops.organization_command_intent WHERE command_id=$1",vec![ack.command_id.into()]).await?;
    if ack.configuration_hash != g.configuration_hash
        || ack.version != g.version
        || ack.status != expected_status
        || intent.try_get::<Uuid>("", "resource_id")? != member
        || intent.try_get::<i64>("", "expected_version")? != g.version
    {
        return Err(conflict());
    }
    let changed=tx.execute(sql("UPDATE kabipay_ops.organization_company SET installed_version=$3,installed_hash=$4,runtime_status=$5,status=CASE WHEN $5='DETACHED' THEN 'DETACHED' ELSE status END,effective_to=CASE WHEN $5='DETACHED' THEN COALESCE(effective_to,NOW()) ELSE effective_to END,updated_at=NOW() WHERE id=$1 AND organization_id=$2 AND version=$3 AND (installed_version IS DISTINCT FROM $3 OR installed_hash IS DISTINCT FROM $4 OR runtime_status<>$5)",vec![member.into(),org.into(),g.version.into(),g.configuration_hash.into(),ack.status.clone().into()])).await?;
    if changed.rows_affected() == 1 {
        event(
            &tx,
            &actor,
            p.id,
            EventSubject {
                organization: Some(org),
                id: member,
                version: g.version,
            },
            "membership.installed",
            json!({"membership_id":member,"status":ack.status,"version":g.version}),
        )
        .await?;
    }
    tx.commit().await?;
    Ok(Json(json!({"acknowledged":true})))
}
