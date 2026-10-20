//! Explicit company HTTP endpoints; this process has no company database secrets.
use crate::{
    identity::{IdentityClient, SourceIdentity},
    runtime_policy::*,
    store::*,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_domain::{Capability, ResourceVersion};
use kabipay_organization_protocol::*;
use kabipay_organization_security::*;
use sea_orm::{ConnectionTrait, TransactionTrait};
use std::{collections::HashMap, sync::Arc};
use uuid::Uuid;
#[derive(Clone)]
pub struct Execution {
    pub(crate) store: Store,
    pub(crate) identities: IdentityClient,
    pub(crate) issuer: Arc<DelegationIssuer>,
    pub(crate) service_issuer: Arc<ServiceCredentialIssuer>,
    pub(crate) service_verifier: Arc<ServiceCredentialVerifier>,
    endpoints: HashMap<Uuid, HashMap<String, reqwest::Url>>,
    http: reqwest::Client,
    pub(crate) reporting: Option<crate::reporting::ReportingClient>,
    pub(crate) archive: Option<crate::archive::ArchiveClient>,
}
fn invalid() -> KabiPayError {
    KabiPayError::Internal("invalid organization execution configuration".into())
}
pub(crate) fn parse_endpoints(
    input: &str,
) -> KabiPayResult<HashMap<Uuid, HashMap<String, reqwest::Url>>> {
    if input.len() > 131072 {
        return Err(invalid());
    }
    let raw: HashMap<Uuid, HashMap<String, String>> =
        serde_json::from_str(input).map_err(|_| invalid())?;
    if raw.is_empty() || raw.len() > 100 {
        return Err(invalid());
    }
    let mut parsed = HashMap::new();
    for (tenant, services) in raw {
        if tenant.is_nil() || services.is_empty() {
            return Err(invalid());
        }
        let mut urls = HashMap::new();
        for (name, value) in services {
            if ![
                "kabipay-employee",
                "kabipay-leave",
                "kabipay-performance",
                "kabipay-attendance",
                "kabipay-payroll",
                "kabipay-workflow",
            ]
            .contains(&name.as_str())
            {
                return Err(invalid());
            }
            let url = reqwest::Url::parse(&value).map_err(|_| invalid())?;
            let loopback = url.host_str().is_some_and(|h| {
                h.parse::<std::net::IpAddr>()
                    .is_ok_and(|ip| ip.is_loopback())
            });
            if !(url.scheme() == "https" || url.scheme() == "http" && loopback)
                || url.host_str().is_none()
                || !url.username().is_empty()
                || url.password().is_some()
                || url.query().is_some()
                || url.fragment().is_some()
                || url.path() != "/internal/organization/command"
            {
                return Err(invalid());
            }
            urls.insert(name, url);
        }
        parsed.insert(tenant, urls);
    }
    Ok(parsed)
}
pub(crate) fn op_code(operation: OrganizationOperation) -> KabiPayResult<String> {
    serde_json::to_value(operation)?
        .as_str()
        .map(str::to_owned)
        .ok_or_else(denied)
}
pub(crate) fn version(v: i64) -> KabiPayResult<ResourceVersion> {
    ResourceVersion::try_from(u64::try_from(v).map_err(|_| conflict())?).map_err(|_| conflict())
}
pub(crate) fn valid_command(c: &OrganizationCommand) -> KabiPayResult<()> {
    valid_id(c.command_id)?;
    valid_id(c.resource_id)?;
    version(c.expected_version)?;
    if !c.payload.is_object() || serde_json::to_vec(c)?.len() > 65536 {
        return Err(KabiPayError::Validation(
            "command payload must be a bounded object".into(),
        ));
    }
    Ok(())
}
pub(crate) async fn resource_employee(
    tx: &sea_orm::DatabaseTransaction,
    org: Uuid,
    member: Uuid,
    c: &OrganizationCommand,
) -> KabiPayResult<Option<Uuid>> {
    if c.operation == OrganizationOperation::FeedbackCreate {
        return Ok(Some(c.resource_id));
    }
    if matches!(
        c.operation,
        OrganizationOperation::LeaveDetail
            | OrganizationOperation::LeaveDecision
            | OrganizationOperation::ReviewDetail
            | OrganizationOperation::ReviewSubmit
            | OrganizationOperation::TimesheetDetail
            | OrganizationOperation::TimesheetDecision
    ) {
        let r=tx.query_one(sql("SELECT i.employee_id FROM kabipay_ops.organization_inbox_projection i JOIN kabipay_ops.organization_company m ON m.tenant_id=i.source_tenant_id AND m.organization_id=i.organization_id WHERE i.organization_id=$1 AND m.id=$2 AND i.resource_type=$3 AND i.resource_id=$4 AND NOT i.deleted",vec![org.into(),member.into(),c.operation.resource_type().into(),c.resource_id.into()])).await?;
        return r
            .map(|r| r.try_get("", "employee_id"))
            .transpose()
            .map_err(Into::into);
    }
    Ok(None)
}
impl Execution {
    pub fn from_env(store: Store, identities: IdentityClient) -> KabiPayResult<Self> {
        let endpoints = parse_endpoints(
            &std::env::var("KABIPAY_ORGANIZATION_RUNTIME_ENDPOINTS").map_err(|_| invalid())?,
        )?;
        Ok(Self {
            store,
            identities,
            issuer: Arc::new(delegation_issuer_from_env().map_err(|_| invalid())?),
            service_issuer: Arc::new(service_issuer_from_env().map_err(|_| invalid())?),
            service_verifier: Arc::new(
                service_verifier_from_env("kabipay-organization").map_err(|_| invalid())?,
            ),
            endpoints,
            http: kabipay_common::organization_http::client(std::time::Duration::from_secs(30))
                .map_err(|_| invalid())?,
            reporting: crate::reporting::ReportingClient::from_env()?,
            archive: crate::archive::ArchiveClient::from_env()?,
        })
    }
    pub async fn execute(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Uuid,
        c: OrganizationCommand,
    ) -> KabiPayResult<OrganizationCommandOutcome> {
        self.execute_inner(actor, org, member, c, None).await
    }
    pub(crate) async fn execute_inner(
        &self,
        actor: &SourceIdentity,
        org: Uuid,
        member: Uuid,
        c: OrganizationCommand,
        original: Option<OrganizationOperation>,
    ) -> KabiPayResult<OrganizationCommandOutcome> {
        if !actor.asymmetric() {
            return Err(denied());
        }
        valid_id(org)?;
        valid_id(member)?;
        valid_command(&c)?;
        let effective = original.unwrap_or(c.operation);
        if matches!(
            effective,
            OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
        ) {
            return Err(denied());
        }
        let body = serde_json::to_vec(&c)?;
        let hash = command_body_hash(&body);
        let tx = self.store.db.begin().await?;
        if matches!(
            effective,
            OrganizationOperation::InstallMembership
                | OrganizationOperation::InstallGrant
                | OrganizationOperation::InstallRelationship
        ) {
            one(&tx,"SELECT id FROM kabipay_ops.organization WHERE id=$1 AND status='ACTIVE' FOR UPDATE",vec![org.into()]).await?;
        }
        let p = current(&tx, actor).await?;
        let (tenant, mv) = if effective == OrganizationOperation::InstallMembership {
            lifecycle_membership(&tx, org, member).await?
        } else {
            membership(&tx, org, member).await?
        };
        let url = self
            .endpoints
            .get(&tenant)
            .and_then(|m| m.get(effective.service()))
            .ok_or_else(invalid)?
            .clone();
        let confirmed_resource = if original.is_some() {
            self.original_resource(&tx, c.resource_id).await?
        } else {
            c.resource_id
        };
        let confirmed_command = if original.is_some() {
            c.resource_id
        } else {
            c.command_id
        };
        let employee = if original.is_some() {
            let row=one(&tx,"SELECT employee_id,operation,request_hash,principal_id,source_tenant_id,source_user_id,organization_id,membership_id FROM kabipay_ops.organization_command_intent WHERE command_id=$1",vec![c.resource_id.into()]).await?;
            if row.try_get::<Uuid>("", "principal_id")? != p.id
                || row.try_get::<Uuid>("", "source_tenant_id")? != actor.tenant()
                || row.try_get::<Uuid>("", "source_user_id")? != actor.user()
                || row.try_get::<Uuid>("", "organization_id")? != org
                || row.try_get::<Uuid>("", "membership_id")? != member
                || row.try_get::<String>("", "operation")? != op_code(effective)?
                || c.payload["request_hash"].as_str()
                    != Some(row.try_get::<String>("", "request_hash")?.as_str())
            {
                return Err(denied());
            }
            row.try_get("", "employee_id")?
        } else {
            resource_employee(&tx, org, member, &c).await?
        };
        let auth = authority(&tx, actor, p.id, org, member, effective, employee).await?;
        match effective {
            OrganizationOperation::InstallMembership => {
                let g = crate::membership_execution::configuration(&tx, org, member).await?;
                if confirmed_resource != member || c.expected_version != g.version {
                    return Err(conflict());
                }
            }
            OrganizationOperation::InstallGrant => {
                let g = grant_configuration(
                    &tx,
                    org,
                    member,
                    if original.is_some() {
                        self.original_resource(&tx, c.resource_id).await?
                    } else {
                        c.resource_id
                    },
                )
                .await?;
                if original.is_none() && g.grant_version != c.expected_version {
                    return Err(conflict());
                }
            }
            OrganizationOperation::InstallRelationship => {
                let g = relationship_configuration(
                    &tx,
                    org,
                    member,
                    if original.is_some() {
                        self.original_resource(&tx, c.resource_id).await?
                    } else {
                        c.resource_id
                    },
                )
                .await?;
                if original.is_none() && g.version != c.expected_version {
                    return Err(conflict());
                }
            }
            OrganizationOperation::Directory
                if c.resource_id != member || c.expected_version != mv =>
            {
                return Err(conflict());
            }
            _ => {}
        }
        let op = op_code(c.operation)?;
        let kind = if auth.kind == AuthorityKind::OrganizationGrant {
            "organization_grant"
        } else {
            "management_assignment"
        };
        if !c.operation.is_read() && original.is_none() {
            tx.execute(sql("INSERT INTO kabipay_ops.organization_command_intent(membership_version,command_id,organization_id,membership_id,tenant_id,principal_id,source_tenant_id,source_user_id,operation,resource_id,expected_version,request_hash,authority_kind,authority_id,authority_version,employee_id,identity_version,source_link_version) VALUES($16,$1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14,$15,$17,$18) ON CONFLICT(command_id) DO NOTHING",vec![c.command_id.into(),org.into(),member.into(),tenant.into(),p.id.into(),actor.tenant().into(),actor.user().into(),op.clone().into(),c.resource_id.into(),c.expected_version.into(),hash.clone().into(),kind.into(),auth.id.into(),auth.version.into(),employee.into(),mv.into(),p.identity_version.into(),p.link_version.into()])).await?;
            let r=one(&tx,"SELECT request_hash,principal_id,source_tenant_id,source_user_id,organization_id,membership_id,authority_id,authority_version FROM kabipay_ops.organization_command_intent WHERE command_id=$1",vec![c.command_id.into()]).await?;
            if r.try_get::<String>("", "request_hash")? != hash
                || r.try_get::<Uuid>("", "principal_id")? != p.id
                || r.try_get::<Uuid>("", "source_tenant_id")? != actor.tenant()
                || r.try_get::<Uuid>("", "source_user_id")? != actor.user()
                || r.try_get::<Uuid>("", "organization_id")? != org
                || r.try_get::<Uuid>("", "membership_id")? != member
                || r.try_get::<Uuid>("", "authority_id")? != auth.id
                || r.try_get::<i64>("", "authority_version")? != auth.version
            {
                return Err(conflict());
            }
        }
        let now = u64::try_from(chrono::Utc::now().timestamp()).map_err(|_| denied())?;
        let claims = DelegationClaims {
            iss: DELEGATION_ISSUER.into(),
            aud: effective.service().into(),
            sub: p.id.try_into().map_err(|_| denied())?,
            jti: Uuid::new_v4(),
            iat: now,
            nbf: now,
            exp: now + 120,
            organization_id: org.try_into().map_err(|_| denied())?,
            target_membership_id: member.try_into().map_err(|_| denied())?,
            source_tenant_id: actor.tenant().try_into().map_err(|_| denied())?,
            source_user_id: actor.user(),
            target_tenant_id: tenant.try_into().map_err(|_| denied())?,
            identity_version: version(p.identity_version)?,
            source_link_version: version(p.link_version)?,
            target_membership_version: version(mv)?,
            authority_kind: auth.kind,
            authority_ref: auth.id,
            authority_version: version(auth.version)?,
            capability: Capability::parse(effective.capability()).map_err(|_| denied())?,
            resource_type: c.operation.resource_type().into(),
            resource_id: c.resource_id,
            expected_resource_version: version(c.expected_version)?,
            command_id: c.command_id,
            request_hash: hash,
        };
        let token = self.issuer.issue(&claims).map_err(|_| denied())?;
        tx.commit().await?;
        let response = self
            .http
            .post(url)
            .bearer_auth(token)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .body(body)
            .send()
            .await;
        let result = crate::source_transport::confirmation(
            response,
            &crate::source_transport::Confirmation {
                command_id: confirmed_command,
                resource_id: confirmed_resource,
                expected_version: c.expected_version,
                receipt_read: original.is_some(),
            },
        )
        .await;
        // Recheck before releasing protected result data; source business effects
        // retain their own receipt even if authority changed during transport.
        let check = self.store.db.begin().await?;
        let current_p = current(&check, actor).await?;
        if current_p.id != p.id
            || current_p.identity_version != p.identity_version
            || current_p.link_version != p.link_version
        {
            return Err(denied());
        }
        let current_auth = authority(&check, actor, p.id, org, member, effective, employee).await?;
        if current_auth.id != auth.id || current_auth.version != auth.version {
            return Err(denied());
        }
        check.commit().await?;
        if !c.operation.is_read() || original.is_some() {
            let update = self.store.db.begin().await?;
            let changed=update.execute(sql("UPDATE kabipay_ops.organization_command_intent SET status=$2,updated_at=NOW() WHERE command_id=$1 AND status IS DISTINCT FROM $2",vec![confirmed_command.into(),result.status.clone().into()])).await?;
            if result.status == "CANCELLED_NOT_EXECUTED" && changed.rows_affected() == 1 {
                event(&update,actor,p.id,EventSubject{organization:Some(org),id:confirmed_command,version:c.expected_version},"command.cancelled",serde_json::json!({"command_id":confirmed_command,"resource_id":confirmed_resource,"membership_id":member,"operation":op_code(effective)?,"version":c.expected_version})).await?;
            }
            update.commit().await?;
        }
        Ok(result)
    }
    async fn original_resource(
        &self,
        tx: &sea_orm::DatabaseTransaction,
        id: Uuid,
    ) -> KabiPayResult<Uuid> {
        Ok(one(
            tx,
            "SELECT resource_id FROM kabipay_ops.organization_command_intent WHERE command_id=$1",
            vec![id.into()],
        )
        .await?
        .try_get("", "resource_id")?)
    }
    pub async fn status(
        &self,
        actor: &SourceIdentity,
        id: Uuid,
    ) -> KabiPayResult<OrganizationCommandOutcome> {
        self.receipt(actor, id, false).await
    }
    pub async fn cancel_pending(
        &self,
        actor: &SourceIdentity,
        id: Uuid,
    ) -> KabiPayResult<OrganizationCommandOutcome> {
        self.receipt(actor, id, true).await
    }
    async fn receipt(
        &self,
        actor: &SourceIdentity,
        id: Uuid,
        cancel: bool,
    ) -> KabiPayResult<OrganizationCommandOutcome> {
        valid_id(id)?;
        let tx = self.store.db.begin().await?;
        let p = current(&tx, actor).await?;
        let r=one(&tx,"SELECT organization_id,membership_id,principal_id,source_tenant_id,source_user_id,operation,request_hash,expected_version,resource_id FROM kabipay_ops.organization_command_intent WHERE command_id=$1",vec![id.into()]).await?;
        if r.try_get::<Uuid>("", "principal_id")? != p.id
            || r.try_get::<Uuid>("", "source_tenant_id")? != actor.tenant()
            || r.try_get::<Uuid>("", "source_user_id")? != actor.user()
        {
            return Err(denied());
        }
        let org = r.try_get("", "organization_id")?;
        let member = r.try_get("", "membership_id")?;
        let operation: OrganizationOperation =
            serde_json::from_value(serde_json::Value::String(r.try_get("", "operation")?))?;
        let hash: String = r.try_get("", "request_hash")?;
        let expected = r.try_get("", "expected_version")?;
        let resource: Uuid = r.try_get("", "resource_id")?;
        tx.commit().await?;
        self.execute_inner(actor,org,member,OrganizationCommand{command_id:Uuid::new_v4(),resource_id:id,expected_version:expected,operation:if cancel{OrganizationOperation::CancelCommand}else{OrganizationOperation::ReadReceipt},payload:if cancel{serde_json::json!({"original_command_id":id,"original_operation":operation,"request_hash":hash,"original_resource_id":resource})}else{serde_json::json!({"original_command_id":id,"original_operation":operation,"request_hash":hash})}},Some(operation)).await
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn endpoints_are_fixed_company_services_with_safe_transports() {
        let id = Uuid::new_v4();
        for url in [
            "http://remote.invalid/internal/organization/command",
            "https://u:p@source.invalid/internal/organization/command",
            "https://source.invalid/other",
            "https://source.invalid/internal/organization/command?q=1",
        ] {
            assert!(parse_endpoints(
                &serde_json::json!({id.to_string():{"kabipay-leave":url}}).to_string()
            )
            .is_err());
        }
        assert!(parse_endpoints(&serde_json::json!({id.to_string():{"kabipay-leave":"https://source.invalid/internal/organization/command"}}).to_string()).is_ok());
    }
}
