//! Private mail transport only. No GraphQL output types or client JWT authority.
use super::EmailEvent;
use crate::{KabiPayError, KabiPayResult};
use chrono::Utc;
use serde::{Deserialize, Serialize};
use uuid::Uuid;

pub const EMAIL_ELIGIBILITY_AUDIENCE: &str = "kabipay-email-eligibility";
pub const EMAIL_RECIPIENT_AUDIENCE: &str = "kabipay-email-recipient";
pub const EMAIL_ELIGIBILITY_ROUTE: &str = "/internal/email/eligibility";
pub const EMAIL_RECIPIENT_ROUTE: &str = "/internal/email/recipient";
pub const EMAIL_PRIVATE_BODY_LIMIT: usize = 16384;
pub const EMAIL_ORGANIZATION_POLICY_AUDIENCE: &str = "kabipay-email-organization-policy";
pub const EMAIL_ORGANIZATION_POLICY_ROUTE: &str = "/internal/email/organization-policy";

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailOrganizationPolicyRequest {
    pub intent: EmailEligibilityRequest,
    /// Derived from the owning domain record, never from a browser or worker.
    pub subject_employee_id: Uuid,
}
impl EmailOrganizationPolicyRequest {
    pub fn validate(&self) -> KabiPayResult<()> {
        use super::{EmailActionTarget, EmailAuthorizationRef, EmailEventKind, EmailRecipientRef};
        self.intent.validate()?;
        let event = &self.intent.event;
        let EmailRecipientRef::OrganizationPrincipal { .. } = event.recipient else {
            return Err(KabiPayError::Unauthorised);
        };
        let EmailActionTarget::OrganizationResource {
            organization_id,
            membership_id,
            ..
        } = event.action_target
        else {
            return Err(KabiPayError::Unauthorised);
        };
        let (auth_org, auth_member) = match event.authorization_ref {
            EmailAuthorizationRef::OrganizationGrant {
                organization_id,
                membership_id,
                ..
            }
            | EmailAuthorizationRef::OrganizationRelationship {
                organization_id,
                membership_id,
                ..
            } => (organization_id, membership_id),
            _ => return Err(KabiPayError::Unauthorised),
        };
        if self.subject_employee_id.is_nil()
            || event.kind != EmailEventKind::ActionRequired
            || auth_org != organization_id
            || auth_member != membership_id
        {
            return Err(KabiPayError::Unauthorised);
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailOrganizationPolicy {
    pub membership_version: i64,
    pub scope: String,
    pub reporting_manager: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailEligibilityRequest {
    pub intent_id: Uuid,
    pub event_hash: String,
    pub event: EmailEvent,
}
impl EmailEligibilityRequest {
    pub fn validate(&self) -> KabiPayResult<()> {
        let now = Utc::now();
        if self.intent_id.is_nil()
            || self.event_hash != self.event.canonical_hash()?
            || self.event.expires_at <= now
            || self.event.occurred_at > now + chrono::Duration::seconds(5)
        {
            return Err(KabiPayError::Unauthorised);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailRecipientIdentity {
    pub source_tenant_id: Uuid,
    pub source_user_id: Uuid,
}

/// Address and trusted login origin exist only in the protected transport and
/// dispatch memory. Debug never reveals them, including inside error wrappers.
#[derive(Serialize)]
pub struct PrivateEmailRecipient {
    pub identity: EmailRecipientIdentity,
    address: String,
    source_login_origin: String,
}
impl std::fmt::Debug for PrivateEmailRecipient {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("PrivateEmailRecipient([redacted])")
    }
}
impl PrivateEmailRecipient {
    pub fn new(
        identity: EmailRecipientIdentity,
        address: String,
        source_login_origin: String,
    ) -> KabiPayResult<Self> {
        let fail = || KabiPayError::Validation("EMAIL_RECIPIENT_UNAVAILABLE".into());
        let address = address.trim().to_owned();
        let mut pieces = address.split('@');
        let local = pieces.next().unwrap_or_default();
        let domain = pieces.next().unwrap_or_default();
        let origin = reqwest::Url::parse(&source_login_origin).map_err(|_| fail())?;
        if identity.source_tenant_id.is_nil()
            || identity.source_user_id.is_nil()
            || address.len() > 254
            || local.is_empty()
            || local.len() > 64
            || domain.is_empty()
            || pieces.next().is_some()
            || !domain.contains('.')
            || !local
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&c))
            || !domain
                .bytes()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'.' | b'-'))
            || origin.scheme() != "https"
            || origin.host_str().is_none()
            || !origin.username().is_empty()
            || origin.password().is_some()
            || origin.query().is_some()
            || origin.fragment().is_some()
            || origin.path() != "/"
        {
            return Err(fail());
        }
        Ok(Self {
            identity,
            address,
            source_login_origin: origin.origin().ascii_serialization(),
        })
    }
    pub fn address(&self) -> &str {
        &self.address
    }
    pub fn source_login_origin(&self) -> &str {
        &self.source_login_origin
    }
}
impl<'de> Deserialize<'de> for PrivateEmailRecipient {
    fn deserialize<D: serde::Deserializer<'de>>(d: D) -> Result<Self, D::Error> {
        #[derive(Deserialize)]
        #[serde(deny_unknown_fields)]
        struct Wire {
            identity: EmailRecipientIdentity,
            address: String,
            source_login_origin: String,
        }
        let wire = Wire::deserialize(d)?;
        Self::new(wire.identity, wire.address, wire.source_login_origin)
            .map_err(|_| serde::de::Error::custom("EMAIL_RECIPIENT_UNAVAILABLE"))
    }
}

/// No address is returned by the owning domain. Only it may issue this token,
/// after matching the persisted event and current business eligibility.
#[derive(Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailEligibility {
    Eligible { recipient_proof: String },
    Obsolete,
    Forbidden,
}
impl std::fmt::Debug for EmailEligibility {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Eligible { .. } => "Eligible([redacted])",
            Self::Obsolete => "Obsolete",
            Self::Forbidden => "Forbidden",
        })
    }
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(tag = "status", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailRecipientResolution {
    Recipient { recipient: PrivateEmailRecipient },
    TestAuthorized { identity: EmailRecipientIdentity },
    Unavailable,
    /// Proof was authenticated and consumed, but current recipient authority
    /// was revoked. Distinct from transient transport/database unavailability.
    Forbidden,
}
