use crate::{KabiPayError, KabiPayResult};
use chrono::NaiveDate;
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Debug, Default, Eq, PartialEq, Serialize, Deserialize)]
pub struct WeeklyOffRule {
    pub fixed_weekdays: Vec<u8>,
    pub saturday_ordinals: Vec<u8>,
}

#[derive(Clone, Debug)]
pub struct LocationVersion {
    pub id: Uuid,
    pub location_id: Option<Uuid>,
    pub effective_from: NaiveDate,
    pub revision: i64,
}

#[derive(Clone, Debug)]
pub struct PolicyVersion {
    pub id: Uuid,
    pub effective_from: NaiveDate,
    pub inherits_default: bool,
    pub rule: WeeklyOffRule,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CalendarDay {
    pub activated: bool,
    pub location_id: Option<Uuid>,
    pub weekly_off: bool,
    pub holiday: bool,
    pub has_roster_work: bool,
    pub policy_version_id: Option<Uuid>,
    pub location_assignment_id: Option<Uuid>,
    pub location_assignment_revision: Option<i64>,
}

#[derive(Clone, Debug)]
pub struct WorkingCalendarSnapshot {
    pub activation_date: Option<NaiveDate>,
    pub revision: Option<i64>,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub employees: HashSet<Uuid>,
    pub assignments: HashMap<Uuid, Vec<LocationVersion>>,
    pub policies: HashMap<Option<Uuid>, Vec<PolicyVersion>>,
    pub holidays: HashSet<(NaiveDate, Option<Uuid>)>,
    pub roster_work: HashSet<(Uuid, NaiveDate)>,
}

impl WorkingCalendarSnapshot {
    pub fn day(&self, employee_id: Uuid, date: NaiveDate) -> KabiPayResult<CalendarDay> {
        if !self.employees.contains(&employee_id) || date < self.from_date || date > self.to_date {
            return Err(KabiPayError::Validation(
                "employee or date is outside the working calendar snapshot".into(),
            ));
        }
        let mut day = CalendarDay {
            activated: self
                .activation_date
                .is_some_and(|activation| date >= activation),
            location_id: None,
            weekly_off: false,
            holiday: false,
            has_roster_work: self.roster_work.contains(&(employee_id, date)),
            policy_version_id: None,
            location_assignment_id: None,
            location_assignment_revision: None,
        };
        if !day.activated {
            return Ok(day);
        }
        if let Some(assignment) = self.assignments.get(&employee_id).and_then(|rows| {
            rows.iter()
                .filter(|row| row.effective_from <= date)
                .max_by_key(|row| row.effective_from)
        }) {
            day.location_id = assignment.location_id;
            day.location_assignment_id = Some(assignment.id);
            day.location_assignment_revision = Some(assignment.revision);
        }
        let default = self.policy(None, date).ok_or_else(|| {
            KabiPayError::Internal("active working calendar has no tenant default".into())
        })?;
        let policy = day
            .location_id
            .and_then(|location| self.policy(Some(location), date))
            .filter(|version| !version.inherits_default)
            .unwrap_or(default);
        day.policy_version_id = Some(policy.id);
        day.weekly_off = !day.has_roster_work && super::is_weekly_off(date, &policy.rule);
        day.holiday = self.holidays.contains(&(date, None))
            || day
                .location_id
                .is_some_and(|location| self.holidays.contains(&(date, Some(location))));
        Ok(day)
    }

    fn policy(&self, location: Option<Uuid>, date: NaiveDate) -> Option<&PolicyVersion> {
        self.policies.get(&location).and_then(|rows| {
            rows.iter()
                .filter(|row| row.effective_from <= date)
                .max_by_key(|row| row.effective_from)
        })
    }
}
