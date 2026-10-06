use super::WeeklyOffRule;
use crate::{KabiPayError, KabiPayResult};
use chrono::{Datelike, NaiveDate, Weekday};
use std::collections::HashSet;

pub fn validate_rule(rule: &WeeklyOffRule) -> KabiPayResult<()> {
    validate_selection(&rule.fixed_weekdays, 7, "weekdays")?;
    validate_selection(&rule.saturday_ordinals, 5, "Saturday occurrences")?;
    if rule.fixed_weekdays.contains(&6) && !rule.saturday_ordinals.is_empty() {
        return Err(KabiPayError::Validation(
            "choose every Saturday or selected Saturdays, not both".into(),
        ));
    }
    Ok(())
}

fn validate_selection(values: &[u8], maximum: u8, label: &str) -> KabiPayResult<()> {
    if values.iter().any(|value| !(1..=maximum).contains(value)) {
        return Err(KabiPayError::Validation(format!(
            "invalid {label} selection"
        )));
    }
    if values.iter().copied().collect::<HashSet<_>>().len() != values.len() {
        return Err(KabiPayError::Validation(format!(
            "duplicate {label} selections are not allowed"
        )));
    }
    Ok(())
}

pub fn is_weekly_off(date: NaiveDate, rule: &WeeklyOffRule) -> bool {
    rule.fixed_weekdays
        .contains(&(date.weekday().number_from_monday() as u8))
        || (date.weekday() == Weekday::Sat
            && rule
                .saturday_ordinals
                .contains(&(1 + (date.day() - 1) as u8 / 7)))
}
