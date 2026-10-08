use crate::services::weekly_off_policy_service::{PolicyState, ScheduleCommand};
use async_graphql::{InputObject, SimpleObject, ID};
use chrono::NaiveDate;
use kabipay_common::{
    working_calendar::{validate_rule, WeeklyOffRule},
    KabiPayError, KabiPayResult,
};

#[derive(InputObject)]
pub struct WeeklyOffRuleInput {
    pub fixed_weekdays: Vec<i32>,
    pub saturday_ordinals: Vec<i32>,
}
impl WeeklyOffRuleInput {
    pub fn rule(self) -> KabiPayResult<WeeklyOffRule> {
        let convert = |items: Vec<i32>| {
            items
                .into_iter()
                .map(|value| {
                    u8::try_from(value).map_err(|_| {
                        KabiPayError::Validation(
                            "weekly-off selection is outside the allowed range".into(),
                        )
                    })
                })
                .collect::<KabiPayResult<Vec<_>>>()
        };
        let rule = WeeklyOffRule {
            fixed_weekdays: convert(self.fixed_weekdays)?,
            saturday_ordinals: convert(self.saturday_ordinals)?,
        };
        validate_rule(&rule)?;
        Ok(rule)
    }
}
#[derive(InputObject)]
pub struct ScheduleWeeklyOffPolicyInput {
    pub location_id: Option<ID>,
    pub effective_from: NaiveDate,
    pub inherits_default: bool,
    pub rule: WeeklyOffRuleInput,
    pub expected_revision: i64,
}
impl ScheduleWeeklyOffPolicyInput {
    pub fn command(self) -> KabiPayResult<ScheduleCommand> {
        Ok(ScheduleCommand {
            location_id: self
                .location_id
                .map(|id| {
                    uuid::Uuid::parse_str(id.as_str())
                        .map_err(|_| KabiPayError::Validation("invalid locationId".into()))
                })
                .transpose()?,
            effective_from: self.effective_from,
            inherits_default: self.inherits_default,
            rule: self.rule.rule()?,
            expected_revision: self.expected_revision,
        })
    }
}
#[derive(SimpleObject)]
pub struct WeeklyOffPolicyVersion {
    pub id: ID,
    pub effective_from: NaiveDate,
    pub inherits_default: bool,
    pub fixed_weekdays: Vec<i32>,
    pub saturday_ordinals: Vec<i32>,
}
#[derive(SimpleObject)]
pub struct WorkingCalendarPolicy {
    pub activation_date: Option<NaiveDate>,
    pub revision: i64,
    pub location_id: Option<ID>,
    pub business_date: NaiveDate,
    pub current_version: Option<WeeklyOffPolicyVersion>,
    pub scheduled_versions: Vec<WeeklyOffPolicyVersion>,
}
pub fn dto(
    state: PolicyState,
    location_id: Option<uuid::Uuid>,
    today: NaiveDate,
) -> KabiPayResult<WorkingCalendarPolicy> {
    let mut current_version = None;
    let mut scheduled_versions = Vec::new();
    for row in state.versions {
        let version = WeeklyOffPolicyVersion {
            id: row.id.into(),
            effective_from: row.effective_from,
            inherits_default: row.inherits_default,
            fixed_weekdays: serde_json::from_value(row.fixed_weekdays)
                .map_err(|_| KabiPayError::Internal("invalid stored weekly-off weekdays".into()))?,
            saturday_ordinals: serde_json::from_value(row.saturday_ordinals)
                .map_err(|_| KabiPayError::Internal("invalid stored Saturday selections".into()))?,
        };
        if version.effective_from <= today {
            current_version = Some(version);
        } else {
            scheduled_versions.push(version);
        }
    }
    Ok(WorkingCalendarPolicy {
        activation_date: state.profile.as_ref().map(|p| p.activation_date),
        revision: state.profile.map_or(0, |p| p.revision),
        location_id: location_id.map(Into::into),
        business_date: today,
        current_version,
        scheduled_versions,
    })
}
