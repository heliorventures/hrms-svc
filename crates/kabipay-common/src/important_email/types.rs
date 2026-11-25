//! Nonsecret, typed business-event metadata. No SMTP credentials or addresses.
use crate::{KabiPayError, KabiPayResult};
use chrono::{DateTime, Duration, Utc};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

macro_rules! wire_enum {
    ($name:ident { $($variant:ident => $value:literal),+ $(,)? }) => {
        #[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
        #[serde(rename_all = "snake_case")]
        pub enum $name { $($variant),+ }
        impl $name { pub const fn as_str(self) -> &'static str { match self { $(Self::$variant => $value),+ } } }
    };
}
wire_enum!(EmailCategory { Leave => "leave", Approval => "approval", Payslip => "payslip",
    MissedPunchOut => "missed_punch_out", Test => "test" });
wire_enum!(EmailEventKind { ActionRequired => "action_required", Outcome => "outcome",
    PayslipAvailable => "payslip_available", MissedPunchOut => "missed_punch_out", Test => "test" });
wire_enum!(EmailResourceType { Leave => "leave", Expense => "expense", Travel => "travel",
    TaxProof => "tax_proof", Timesheet => "timesheet", ProfileChange => "profile_change",
    LoanRequest => "loan_request", PerformanceReview => "performance_review", Payslip => "payslip",
    Attendance => "attendance", Test => "test" });

pub type EmailResourceVersion = String;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailRecipientRef {
    LocalUser {
        user_id: Uuid,
    },
    OrganizationPrincipal {
        principal_id: Uuid,
        source_tenant_id: Uuid,
        source_employee_id: Uuid,
    },
}
/// Actor identity is not a destination. A foreign business command already
/// proves its principal; it need not invent an employee routing reference.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailActorRef {
    LocalUser { user_id: Uuid },
    OrganizationPrincipal { principal_id: Uuid },
}
impl EmailActorRef {
    pub fn valid(&self) -> bool {
        match self {
            Self::LocalUser { user_id } => !user_id.is_nil(),
            Self::OrganizationPrincipal { principal_id } => !principal_id.is_nil(),
        }
    }
}
impl From<EmailRecipientRef> for EmailActorRef {
    fn from(value: EmailRecipientRef) -> Self {
        match value {
            EmailRecipientRef::LocalUser { user_id } => Self::LocalUser { user_id },
            EmailRecipientRef::OrganizationPrincipal { principal_id, .. } => {
                Self::OrganizationPrincipal { principal_id }
            }
        }
    }
}

/// Producer-side selection result, never an authority proof or a mailbox.
/// Tasks which commit the business transition construct these from current
/// bounded permission/workflow and protected source-employment reads.
pub struct EmailReviewer {
    pub recipient: EmailRecipientRef,
    pub authorization_ref: EmailAuthorizationRef,
    pub action_target: EmailActionTarget,
}

impl EmailRecipientRef {
    pub fn key(&self) -> String {
        match self {
            Self::LocalUser { user_id } => format!("local:{user_id}"),
            Self::OrganizationPrincipal {
                principal_id,
                source_tenant_id,
                source_employee_id,
            } => format!("organization:{principal_id}:{source_tenant_id}:{source_employee_id}"),
        }
    }
    pub fn valid(&self) -> bool {
        match self {
            Self::LocalUser { user_id } => !user_id.is_nil(),
            Self::OrganizationPrincipal {
                principal_id,
                source_tenant_id,
                source_employee_id,
            } => ![principal_id, source_tenant_id, source_employee_id]
                .iter()
                .any(|id| id.is_nil()),
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailActionTarget {
    CompanyResource {
        resource_type: EmailResourceType,
        resource_id: Uuid,
    },
    OrganizationResource {
        organization_id: Uuid,
        membership_id: Uuid,
        resource_type: EmailResourceType,
        resource_id: Uuid,
    },
}

impl EmailActionTarget {
    fn matches(&self, kind: EmailResourceType, id: Uuid) -> bool {
        match self {
            Self::CompanyResource {
                resource_type,
                resource_id,
            } => *resource_type == kind && *resource_id == id,
            Self::OrganizationResource {
                organization_id,
                membership_id,
                resource_type,
                resource_id,
            } => {
                !organization_id.is_nil()
                    && !membership_id.is_nil()
                    && *resource_type == kind
                    && *resource_id == id
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
pub enum EmailAuthorizationRef {
    LocalWorkflow {
        instance_id: Uuid,
        step_id: Uuid,
    },
    DomainTransition {
        resource_type: EmailResourceType,
        transition_key: String,
    },
    OrganizationGrant {
        organization_id: Uuid,
        membership_id: Uuid,
        binding_id: Uuid,
        version: i64,
    },
    OrganizationRelationship {
        organization_id: Uuid,
        membership_id: Uuid,
        relationship_id: Uuid,
        version: i64,
    },
}

impl EmailAuthorizationRef {
    fn valid(&self, resource: EmailResourceType) -> bool {
        match self {
            Self::LocalWorkflow {
                instance_id,
                step_id,
            } => !instance_id.is_nil() && !step_id.is_nil(),
            Self::DomainTransition {
                resource_type,
                transition_key,
            } => {
                *resource_type == resource
                    && !transition_key.is_empty()
                    && transition_key.len() <= 256
            }
            Self::OrganizationGrant {
                organization_id,
                membership_id,
                binding_id,
                version,
            } => {
                ![organization_id, membership_id, binding_id]
                    .iter()
                    .any(|id| id.is_nil())
                    && *version > 0
            }
            Self::OrganizationRelationship {
                organization_id,
                membership_id,
                relationship_id,
                version,
            } => {
                ![organization_id, membership_id, relationship_id]
                    .iter()
                    .any(|id| id.is_nil())
                    && *version > 0
            }
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmailEvent {
    pub event_id: Uuid,
    pub tenant_id: Uuid,
    pub resource_type: EmailResourceType,
    pub resource_id: Uuid,
    pub resource_version: EmailResourceVersion,
    pub category: EmailCategory,
    pub kind: EmailEventKind,
    pub template_version: u16,
    pub recipient: EmailRecipientRef,
    pub actor_ref: Option<EmailActorRef>,
    pub authorization_ref: EmailAuthorizationRef,
    pub action_target: EmailActionTarget,
    pub occurred_at: DateTime<Utc>,
    pub expires_at: DateTime<Utc>,
}

impl EmailEvent {
    pub fn required_module(&self) -> &'static str {
        use EmailResourceType::*;
        crate::entitlements::permission_module(match self.resource_type {
            Leave => "leave:read",
            Expense => "expense:read",
            Travel => "travel:read",
            TaxProof => "tax:read",
            Timesheet => "timesheet:read",
            ProfileChange => "employee:read",
            LoanRequest => "loan:read",
            PerformanceReview => "performance:evaluate",
            Payslip => "payroll:read",
            Attendance => "attendance:read",
            Test => "email_settings:read",
        })
    }

    pub fn validate(&self) -> KabiPayResult<()> {
        let ttl = self.expires_at.signed_duration_since(self.occurred_at);
        let maximum = if self.kind == EmailEventKind::ActionRequired {
            Duration::hours(24)
        } else {
            Duration::days(7)
        };
        if [self.event_id, self.tenant_id, self.resource_id]
            .iter()
            .any(Uuid::is_nil)
            || self.resource_version.is_empty()
            || self.resource_version.len() > 256
            || self.template_version != 1
            || !self.recipient.valid()
            || self.actor_ref.as_ref().is_some_and(|actor| !actor.valid())
            || !self.authorization_ref.valid(self.resource_type)
            || !self
                .action_target
                .matches(self.resource_type, self.resource_id)
            || ttl <= Duration::zero()
            || ttl > maximum
            || !self.valid_category()
        {
            return Err(KabiPayError::Validation(
                "invalid important email event".into(),
            ));
        }
        Ok(())
    }

    fn valid_category(&self) -> bool {
        use EmailCategory as Category;
        use EmailEventKind as Kind;
        use EmailResourceType as Resource;
        match (self.category, self.resource_type, self.kind) {
            (Category::Leave, Resource::Leave, Kind::ActionRequired | Kind::Outcome) => true,
            (
                Category::Approval,
                Resource::Expense
                | Resource::Travel
                | Resource::TaxProof
                | Resource::Timesheet
                | Resource::ProfileChange
                | Resource::LoanRequest
                | Resource::PerformanceReview,
                Kind::ActionRequired | Kind::Outcome,
            ) => true,
            (Category::Payslip, Resource::Payslip, Kind::PayslipAvailable) => true,
            (Category::MissedPunchOut, Resource::Attendance, Kind::MissedPunchOut) => true,
            (Category::Test, Resource::Test, Kind::Test) => true,
            _ => false,
        }
    }

    /// Canonical persisted PostgreSQL microsecond precision, including embedded JSON.
    pub fn canonical(&self) -> KabiPayResult<Self> {
        self.validate()?;
        let mut event = self.clone();
        event.occurred_at = DateTime::from_timestamp_micros(self.occurred_at.timestamp_micros())
            .ok_or_else(|| KabiPayError::Validation("invalid email timestamp".into()))?;
        event.expires_at = DateTime::from_timestamp_micros(self.expires_at.timestamp_micros())
            .ok_or_else(|| KabiPayError::Validation("invalid email timestamp".into()))?;
        Ok(event)
    }

    pub fn canonical_hash(&self) -> KabiPayResult<String> {
        let encoded = serde_json::to_vec(&self.canonical()?)?;
        Ok(format!("{:x}", Sha256::digest(encoded)))
    }

    pub const fn producer(&self) -> &'static str {
        use EmailResourceType as Resource;
        match self.resource_type {
            Resource::Leave => "leave",
            Resource::Expense | Resource::Travel => "expense",
            Resource::TaxProof => "tax",
            Resource::Timesheet | Resource::Attendance => "attendance",
            Resource::ProfileChange => "employee",
            Resource::LoanRequest | Resource::Payslip => "payroll",
            Resource::PerformanceReview => "performance",
            Resource::Test => "notification",
        }
    }
}

/// Deterministic transition identity; recipient is separate so dispatch can
/// deduplicate a person resolved through local and organization references.
pub fn stable_event_id(
    tenant: Uuid,
    resource: EmailResourceType,
    id: Uuid,
    version: &str,
    kind: EmailEventKind,
) -> Uuid {
    let mut hash = Sha256::new();
    for part in [
        "tenant-email-event-v1",
        &tenant.to_string(),
        resource.as_str(),
        &id.to_string(),
        version,
        kind.as_str(),
    ] {
        hash.update((part.len() as u64).to_be_bytes());
        hash.update(part.as_bytes());
    }
    let digest = hash.finalize();
    let mut bytes = [0; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x80;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    Uuid::from_bytes(bytes)
}
