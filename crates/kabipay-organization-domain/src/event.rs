use crate::{OrganizationError, PrincipalId, ResourceVersion, TenantId};
use chrono::{DateTime, Utc};
use serde::{Deserialize, Deserializer, Serialize, Serializer};
use uuid::Uuid;

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum EventActor {
    Person {
        principal_id: PrincipalId,
        login_tenant_id: TenantId,
        authority_ref: Uuid,
        authority_version: ResourceVersion,
    },
    System {
        service: String,
    },
}
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BusinessEventHeaderInput {
    pub event_id: Uuid,
    pub event_type: String,
    pub schema_version: u32,
    pub organization_id: Uuid,
    pub organization_membership_id: Uuid,
    pub source_tenant_id: Uuid,
    pub aggregate_type: String,
    pub aggregate_id: Uuid,
    pub source_version: u64,
    pub actor: EventActor,
    pub command_id: Option<Uuid>,
    pub correlation_id: Uuid,
    pub causation_id: Option<Uuid>,
    pub occurred_at: DateTime<Utc>,
}
#[derive(Debug, Clone)]
pub struct BusinessEventHeader(BusinessEventHeaderInput);
pub const BUSINESS_EVENT_SCHEMA_VERSION: u32 = 1;
impl TryFrom<BusinessEventHeaderInput> for BusinessEventHeader {
    type Error = OrganizationError;
    fn try_from(value: BusinessEventHeaderInput) -> Result<Self, Self::Error> {
        if [
            value.event_id,
            value.organization_id,
            value.organization_membership_id,
            value.source_tenant_id,
            value.aggregate_id,
            value.correlation_id,
        ]
        .iter()
        .any(Uuid::is_nil)
            || value.command_id.is_some_and(|id| id.is_nil())
            || value.causation_id.is_some_and(|id| id.is_nil())
            || value.schema_version != BUSINESS_EVENT_SCHEMA_VERSION
            || !event_name(&value.event_type)
            || !resource_name(&value.aggregate_type)
        {
            return Err(OrganizationError::InvalidEvent);
        }
        ResourceVersion::try_from(value.source_version)?;
        match &value.actor {
            EventActor::Person { authority_ref, .. } if authority_ref.is_nil() => {
                return Err(OrganizationError::InvalidEvent)
            }
            EventActor::System { service } if !resource_name(service) => {
                return Err(OrganizationError::InvalidEvent)
            }
            _ => {}
        }
        Ok(Self(value))
    }
}
impl BusinessEventHeader {
    /// Check transport-configured company identity independently of event payload claims.
    /// # Errors
    /// Returns `WrongTenant` for a source not bound to the authenticated producer/runtime.
    pub fn require_source(&self, expected: TenantId) -> Result<(), OrganizationError> {
        if self.0.source_tenant_id != Uuid::from(expected) {
            return Err(OrganizationError::WrongTenant);
        }
        Ok(())
    }
}
fn event_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.as_bytes()[0].is_ascii_alphabetic()
        && value
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'_' | b'-' | b'.'))
}
fn resource_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 100
        && value.as_bytes()[0].is_ascii_lowercase()
        && value.bytes().all(|b| {
            b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-' | b'.')
        })
}
impl Serialize for BusinessEventHeader {
    fn serialize<S: Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.0.serialize(serializer)
    }
}
impl<'de> Deserialize<'de> for BusinessEventHeader {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        Self::try_from(BusinessEventHeaderInput::deserialize(deserializer)?)
            .map_err(serde::de::Error::custom)
    }
}
/// Payloads are typed by each feature. Transport authentication remains separate.
#[derive(Debug, Clone, Serialize)]
pub struct BusinessEvent<P> {
    #[serde(flatten)]
    pub header: BusinessEventHeader,
    pub payload: P,
}
