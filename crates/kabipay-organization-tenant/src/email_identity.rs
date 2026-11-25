//! Protected source identity lookup. Only an owning-domain mail proof reaches
//! this boundary. Notification never receives a general user-directory API.
use crate::{email_transport::VerifiedEmailRecipient, statement};
use kabipay_common::{context::ScopeType, important_email::*, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, Value};
use uuid::Uuid;

fn denied() -> KabiPayError {
    KabiPayError::Unauthorised
}

/// Rechecks only current metadata; does not retrieve any address or profile.
async fn organization_user(ops: &impl ConnectionTrait, event: &EmailEvent) -> KabiPayResult<Uuid> {
    let EmailRecipientRef::OrganizationPrincipal { principal_id, .. } = event.recipient else {
        return Err(denied());
    };
    let EmailActionTarget::OrganizationResource {
        organization_id,
        membership_id,
        ..
    } = event.action_target
    else {
        return Err(denied());
    };
    let user = current_email_organization_user(ops, event).await?;
    if event.kind == EmailEventKind::ActionRequired {
        let capability = email_action_permission(event.resource_type).ok_or_else(denied)?;
        let current = match event.authorization_ref {
          EmailAuthorizationRef::OrganizationGrant { organization_id: auth_org, membership_id: auth_member, binding_id, version } if auth_org == organization_id && auth_member == membership_id => ops.query_one(statement(
            "SELECT b.id FROM kabipay_ops.principal_role_binding b JOIN kabipay_ops.organization_role r ON r.id=b.role_id AND r.organization_id=b.organization_id JOIN kabipay_ops.organization_role_capability c ON c.role_id=r.id AND c.organization_id=r.organization_id JOIN kabipay_ops.role_binding_company s ON s.binding_id=b.id AND s.organization_id=b.organization_id WHERE b.id=$1 AND b.organization_id=$2 AND b.principal_id=$3 AND b.version=$4 AND b.status='ACTIVE' AND r.status='ACTIVE' AND b.effective_from<=clock_timestamp() AND (b.effective_to IS NULL OR b.effective_to>clock_timestamp()) AND s.organization_company_id=$5 AND s.activation_status='ACTIVE' AND s.expected_grant_version=b.version AND c.capability_code=$6 AND c.employee_scope IN ('ALL','DEPARTMENT','SELF')",
            vec![binding_id.into(), organization_id.into(), principal_id.into(), version.into(), membership_id.into(), capability.into()])).await?,
          EmailAuthorizationRef::OrganizationRelationship { organization_id: auth_org, membership_id: auth_member, relationship_id, version } if auth_org == organization_id && auth_member == membership_id => ops.query_one(statement(
            "SELECT a.id FROM kabipay_ops.organization_relationship a JOIN kabipay_ops.employment_ref manager ON manager.id=a.manager_employment_ref_id AND manager.organization_id=a.organization_id JOIN kabipay_ops.employment_ref subject ON subject.id=a.subject_employment_ref_id AND subject.organization_id=a.organization_id WHERE a.id=$1 AND a.organization_id=$2 AND a.organization_company_id=$3 AND manager.principal_id=$4 AND a.version=$5 AND a.installed_version=a.version AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND a.capabilities @> $6::jsonb AND manager.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE') AND subject.employment_status IN ('ACTIVE','PROBATION','ON_LEAVE')",
            vec![relationship_id.into(),organization_id.into(),membership_id.into(),principal_id.into(),version.into(),serde_json::json!([capability]).into()])).await?,
          _ => return Err(denied()),
        };
        if current.is_none() {
            return Err(denied());
        }
    }
    Ok(user)
}

pub async fn resolve_email_recipient(
    source: &impl ConnectionTrait,
    ops: &impl ConnectionTrait,
    proof: &VerifiedEmailRecipient,
    source_login_origin: &str,
) -> KabiPayResult<EmailRecipientResolution> {
    let event = &proof.request.event;
    // A slow metadata lookup must never extend a proof beyond its original TTL.
    proof.request.validate()?;
    let source_tenant = proof.source_tenant_id();
    let (user, employee): (Uuid, Option<Uuid>) = match event.recipient {
        EmailRecipientRef::LocalUser { user_id } => (user_id, None),
        EmailRecipientRef::OrganizationPrincipal {
            source_employee_id, ..
        } => (
            organization_user(ops, event).await?,
            Some(source_employee_id),
        ),
    };
    if event.kind == EmailEventKind::Test {
        // Notification may authorize ONLY its fixed settings test; this branch
        // returns no mailbox, even to a correctly signed Notification request.
        if employee.is_some() || source_tenant != event.tenant_id {
            return Err(denied());
        }
        for permission in ["email_settings:read", "email_settings:test"] {
            if current_email_permission_scope(source, source_tenant, user, permission).await?
                != Some(ScopeType::All)
            {
                return Err(denied());
            }
        }
        return Ok(EmailRecipientResolution::TestAuthorized {
            identity: EmailRecipientIdentity {
                source_tenant_id: source_tenant,
                source_user_id: user,
            },
        });
    }
    let mut values: Vec<Value> = vec![source_tenant.into(), user.into()];
    let filter = if let Some(employee) = employee {
        values.push(employee.into());
        " AND e.id=$3"
    } else {
        ""
    };
    let rows = if employee.is_none() && event.kind==EmailEventKind::ActionRequired
        && matches!(event.resource_type,kabipay_common::important_email::EmailResourceType::ProfileChange | kabipay_common::important_email::EmailResourceType::PerformanceReview) {
        source.query_all(statement("SELECT u.id,u.email FROM \"user\" u WHERE u.tenant_id=$1 AND u.id=$2 AND u.is_active AND NOT u.is_deleted LIMIT 2",values)).await?
    } else {
        source.query_all(statement(&format!(
        "SELECT u.id,u.email FROM \"user\" u JOIN employee e ON e.user_id=u.id AND e.tenant_id=u.tenant_id WHERE u.tenant_id=$1 AND u.id=$2 AND u.is_active AND NOT u.is_deleted AND NOT e.is_deleted AND e.status IN ('ACTIVE','PROBATION','ON_LEAVE'){filter} LIMIT 2"), values)).await?
    };
    if rows.len() != 1 || rows[0].try_get::<Uuid>("", "id")? != user {
        return Err(denied());
    }
    let Some(address): Option<String> = rows[0].try_get("", "email")? else {
        return Ok(EmailRecipientResolution::Unavailable);
    };
    if address.trim().is_empty() {
        return Ok(EmailRecipientResolution::Unavailable);
    }
    proof.request.validate()?;
    if proof.credential.claims().exp
        <= u64::try_from(chrono::Utc::now().timestamp()).map_err(|_| denied())?
    {
        return Err(denied());
    }
    let recipient = PrivateEmailRecipient::new(
        EmailRecipientIdentity {
            source_tenant_id: source_tenant,
            source_user_id: user,
        },
        address,
        source_login_origin.into(),
    )?;
    Ok(EmailRecipientResolution::Recipient { recipient })
}
