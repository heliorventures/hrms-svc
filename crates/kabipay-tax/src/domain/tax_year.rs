use super::validate_year;
use chrono::{Datelike, Duration, NaiveDate};
use kabipay_common::{KabiPayError, KabiPayResult};
#[derive(Clone, Debug)]
pub struct EmploymentMonth {
    pub start: NaiveDate,
    pub end: NaiveDate,
    pub days: u32,
    pub calendar_days: u32,
}
pub fn month_bounds(year: i32, month: u32) -> KabiPayResult<(NaiveDate, NaiveDate)> {
    let start = NaiveDate::from_ymd_opt(year, month, 1)
        .ok_or_else(|| KabiPayError::Validation("invalid payroll month".into()))?;
    let next = if month == 12 {
        NaiveDate::from_ymd_opt(year + 1, 1, 1)
    } else {
        NaiveDate::from_ymd_opt(year, month + 1, 1)
    }
    .ok_or_else(|| KabiPayError::Validation("invalid payroll year".into()))?;
    Ok((start, next - Duration::days(1)))
}
pub fn employment_months(
    year: i32,
    joining: NaiveDate,
    exit: Option<NaiveDate>,
) -> KabiPayResult<Vec<EmploymentMonth>> {
    validate_year(year)?;
    if exit.is_some_and(|end| end < joining) {
        return Err(KabiPayError::Validation(
            "exit precedes joining date".into(),
        ));
    }
    let mut result = Vec::new();
    for offset in 0..12 {
        let month = (offset + 3) % 12 + 1;
        let (start, end) = month_bounds(year + if month < 4 { 1 } else { 0 }, month)?;
        let active_start = start.max(joining);
        let active_end = exit.map_or(end, |v| v.min(end));
        if active_start <= active_end {
            result.push(EmploymentMonth {
                start: active_start,
                end: active_end,
                days: (active_end - active_start).num_days() as u32 + 1,
                calendar_days: end.day(),
            });
        }
    }
    Ok(result)
}
