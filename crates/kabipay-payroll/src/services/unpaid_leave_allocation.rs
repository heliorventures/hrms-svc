//! Allocate approved leave to actual payroll dates; never prorate a spanning request.
use chrono::{Datelike, NaiveDate, Weekday};
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use std::collections::HashSet;

pub fn allocate_dates(
    from: NaiveDate, to: NaiveDate, half_day: bool, sandwich: bool,
    approved_days: Decimal, holidays: &HashSet<NaiveDate>,
) -> KabiPayResult<Vec<(NaiveDate, Decimal)>> {
    if from > to || (to - from).num_days() > 3660 || (half_day && from != to) {
        return Err(KabiPayError::Validation("invalid unpaid leave date range; HR review required".into()));
    }
    let mut result = Vec::new();
    let mut date = from;
    loop {
        if half_day || sandwich || (!matches!(date.weekday(), Weekday::Sat | Weekday::Sun) && !holidays.contains(&date)) {
            result.push((date, if half_day { Decimal::new(5, 1) } else { Decimal::ONE }));
        }
        if date == to { break; }
        date = date.succ_opt().ok_or_else(|| KabiPayError::Validation("leave date exceeds supported range".into()))?;
    }
    if result.iter().map(|(_, days)| *days).sum::<Decimal>() != approved_days {
        return Err(KabiPayError::Validation("approved unpaid leave days do not match its dated allocation; HR must review the request and calendar before payroll".into()));
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn date(value: &str) -> NaiveDate { value.parse().unwrap() }
    #[test]
    fn spanning_request_charges_actual_dates_in_each_month() {
        let rows = allocate_dates(date("2026-09-30"), date("2026-10-02"), false, false, Decimal::from(2), &HashSet::from([date("2026-10-02")])).unwrap();
        assert_eq!(rows, vec![(date("2026-09-30"), Decimal::ONE), (date("2026-10-01"), Decimal::ONE)]);
    }
    #[test]
    fn sandwich_includes_weekends_and_holidays() {
        assert_eq!(allocate_dates(date("2026-09-05"), date("2026-09-07"), false, true, Decimal::from(3), &HashSet::from([date("2026-09-07")])).unwrap().len(), 3);
    }
    #[test]
    fn half_day_keeps_approved_leave_engine_semantics() {
        assert_eq!(allocate_dates(date("2026-09-06"), date("2026-09-06"), true, false, Decimal::new(5, 1), &HashSet::new()).unwrap()[0].1, Decimal::new(5, 1));
    }
    #[test]
    fn historical_count_mismatch_fails_instead_of_silent_proration() {
        assert!(allocate_dates(date("2026-09-05"), date("2026-09-07"), false, false, Decimal::from(3), &HashSet::new()).is_err());
    }
    #[test]
    fn malformed_ranges_are_rejected() {
        assert!(allocate_dates(date("2026-09-02"), date("2026-09-01"), false, true, Decimal::ONE, &HashSet::new()).is_err());
        assert!(allocate_dates(date("2026-09-01"), date("2026-09-02"), true, true, Decimal::ONE, &HashSet::new()).is_err());
    }
}
