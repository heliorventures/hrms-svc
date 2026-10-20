use crate::OrganizationError;
use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

macro_rules! identifier {
    ($name:ident) => {
        #[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
        #[serde(try_from = "Uuid", into = "Uuid")]
        pub struct $name(Uuid);
        impl TryFrom<Uuid> for $name {
            type Error = OrganizationError;
            fn try_from(value: Uuid) -> Result<Self, Self::Error> {
                if value.is_nil() {
                    return Err(OrganizationError::InvalidIdentifier);
                }
                Ok(Self(value))
            }
        }
        impl From<$name> for Uuid {
            fn from(value: $name) -> Self {
                value.0
            }
        }
    };
}
identifier!(OrganizationId);
identifier!(TenantId);
identifier!(MembershipId);
identifier!(PrincipalId);
identifier!(EmploymentId);

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "u64", into = "u64")]
pub struct ResourceVersion(u64);
impl TryFrom<u64> for ResourceVersion {
    type Error = OrganizationError;
    fn try_from(value: u64) -> Result<Self, Self::Error> {
        if value == 0 || value > i64::MAX as u64 {
            return Err(OrganizationError::InvalidVersion);
        }
        Ok(Self(value))
    }
}
impl From<ResourceVersion> for u64 {
    fn from(value: ResourceVersion) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "String", into = "String")]
pub struct Capability(String);
impl Capability {
    /// Parse an exact canonical capability; catalog support is checked by the policy service.
    ///
    /// # Errors
    /// Rejects wildcards, whitespace, case aliases and malformed resource/action names.
    pub fn parse(value: impl Into<String>) -> Result<Self, OrganizationError> {
        let value = value.into();
        let Some((resource, action)) = value.split_once(':') else {
            return Err(OrganizationError::InvalidCapability);
        };
        if value.len() > 100 || !canonical_segment(resource) || !canonical_segment(action) {
            return Err(OrganizationError::InvalidCapability);
        }
        Ok(Self(value))
    }
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}
fn canonical_segment(value: &str) -> bool {
    value.as_bytes().first().is_some_and(u8::is_ascii_lowercase)
        && value
            .bytes()
            .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || matches!(b, b'_' | b'-'))
}
impl TryFrom<String> for Capability {
    type Error = OrganizationError;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::parse(value)
    }
}
impl From<Capability> for String {
    fn from(value: Capability) -> Self {
        value.0
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "PeriodInput", into = "PeriodInput")]
pub struct EffectivePeriod {
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
}
#[derive(Debug, Clone, Copy, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct PeriodInput {
    from: DateTime<Utc>,
    to: Option<DateTime<Utc>>,
}
impl EffectivePeriod {
    /// # Errors
    /// Rejects empty or reversed periods. End timestamps are exclusive.
    pub fn new(from: DateTime<Utc>, to: Option<DateTime<Utc>>) -> Result<Self, OrganizationError> {
        if to.is_some_and(|end| end <= from) {
            return Err(OrganizationError::InvalidPeriod);
        }
        Ok(Self { from, to })
    }
    #[must_use]
    pub fn contains(&self, at: DateTime<Utc>) -> bool {
        self.from <= at && self.to.is_none_or(|end| at < end)
    }
}
impl TryFrom<PeriodInput> for EffectivePeriod {
    type Error = OrganizationError;
    fn try_from(value: PeriodInput) -> Result<Self, Self::Error> {
        Self::new(value.from, value.to)
    }
}
impl From<EffectivePeriod> for PeriodInput {
    fn from(value: EffectivePeriod) -> Self {
        Self {
            from: value.from,
            to: value.to,
        }
    }
}
