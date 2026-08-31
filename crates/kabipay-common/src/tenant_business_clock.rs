//! Tenant-local business time backed by an IANA timezone identifier.

use chrono::{DateTime, LocalResult, NaiveDate, NaiveDateTime, NaiveTime, TimeZone, Utc};
use chrono_tz::Tz;
use kabipay_db_entities::ops::tenant;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

use crate::{KabiPayError, KabiPayResult};

const DEFAULT_TENANT_TIMEZONE: &str = "UTC";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct TenantBusinessClock {
    timezone: Tz,
}

impl TenantBusinessClock {
    pub fn from_configured_name(name: Option<&str>) -> KabiPayResult<Self> {
        Self::from_name(name.unwrap_or(DEFAULT_TENANT_TIMEZONE))
    }

    pub fn from_name(name: &str) -> KabiPayResult<Self> {
        let normalized = name.trim();
        if normalized.is_empty() {
            return Err(KabiPayError::Validation(
                "tenant timezone must be an IANA timezone identifier".into(),
            ));
        }
        let timezone = normalized.parse::<Tz>().map_err(|_| {
            KabiPayError::Validation(
                "tenant timezone must be an IANA timezone identifier".into(),
            )
        })?;
        Ok(Self { timezone })
    }

    pub async fn load(ops_db: &DatabaseConnection, tenant_id: Uuid) -> KabiPayResult<Self> {
        let row = tenant::Entity::find_by_id(tenant_id)
            .filter(tenant::Column::IsDeleted.eq(false))
            .one(ops_db)
            .await?
            .ok_or_else(|| KabiPayError::TenantNotFound(tenant_id.to_string()))?;
        Self::from_configured_name(row.timezone.as_deref())
    }

    pub fn timezone_name(self) -> &'static str {
        self.timezone.name()
    }

    pub fn business_date(self, instant: DateTime<Utc>) -> NaiveDate {
        instant.with_timezone(&self.timezone).date_naive()
    }

    pub fn local_time(self, instant: DateTime<Utc>) -> NaiveTime {
        instant.with_timezone(&self.timezone).time()
    }

    pub fn now_date(self) -> NaiveDate {
        self.business_date(Utc::now())
    }

    pub fn to_utc(self, date: NaiveDate, time: NaiveTime) -> KabiPayResult<DateTime<Utc>> {
        let local = NaiveDateTime::new(date, time);
        match self.timezone.from_local_datetime(&local) {
            LocalResult::Single(value) => Ok(value.with_timezone(&Utc)),
            LocalResult::Ambiguous(_, _) => Err(KabiPayError::Validation(format!(
                "{date} {time} is ambiguous in tenant timezone {}",
                self.timezone.name()
            ))),
            LocalResult::None => Err(KabiPayError::Validation(format!(
                "{date} {time} does not exist in tenant timezone {}",
                self.timezone.name()
            ))),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn utc(value: &str) -> DateTime<Utc> {
        value.parse().expect("test timestamp must be valid")
    }

    fn date(value: &str) -> NaiveDate {
        value.parse().expect("test date must be valid")
    }

    fn time(value: &str) -> NaiveTime {
        value.parse().expect("test time must be valid")
    }

    #[test]
    fn business_date_uses_tenant_zone_across_utc_midnight_boundary() {
        let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
        assert_eq!(clock.business_date(utc("2026-08-23T19:00:00Z")), date("2026-08-24"));
    }

    #[test]
    fn nonexistent_and_ambiguous_dst_times_are_rejected() {
        let clock = TenantBusinessClock::from_name("America/New_York").unwrap();
        assert!(clock.to_utc(date("2026-03-08"), time("02:30:00")).is_err());
        assert!(clock.to_utc(date("2026-11-01"), time("01:30:00")).is_err());
    }

    #[test]
    fn invalid_or_blank_timezone_is_rejected() {
        assert!(TenantBusinessClock::from_name("").is_err());
        assert!(TenantBusinessClock::from_name("Asia/Not-A-Zone").is_err());
    }

    #[test]
    fn missing_configuration_uses_the_documented_utc_default() {
        let clock = TenantBusinessClock::from_configured_name(None).unwrap();

        assert_eq!(clock.timezone_name(), "UTC");
    }
}
