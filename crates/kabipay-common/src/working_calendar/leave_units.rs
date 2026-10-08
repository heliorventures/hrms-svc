use crate::{KabiPayError, KabiPayResult};
use chrono::NaiveDate;
use rust_decimal::Decimal;
use serde::Deserialize;

#[derive(Deserialize)]
struct DateUnit {
    date: NaiveDate,
    units: String,
}

pub fn decode_leave_units(
    value: serde_json::Value,
    from: NaiveDate,
    to: NaiveDate,
    expected: Decimal,
) -> KabiPayResult<Vec<(NaiveDate, Decimal)>> {
    let invalid = || {
        KabiPayError::Validation(
            "stored leave dates do not match the approved request; HR review required".into(),
        )
    };
    let entries: Vec<DateUnit> = serde_json::from_value(value).map_err(|_| invalid())?;
    let mut result = Vec::new();
    let mut previous = None;
    for entry in entries {
        let units = entry.units.parse::<Decimal>().map_err(|_| invalid())?;
        if entry.date < from
            || entry.date > to
            || previous.is_some_and(|date| entry.date <= date)
            || (units != Decimal::ONE && units != Decimal::new(5, 1))
        {
            return Err(invalid());
        }
        previous = Some(entry.date);
        result.push((entry.date, units));
    }
    if result.is_empty() || result.iter().map(|(_, units)| *units).sum::<Decimal>() != expected {
        return Err(invalid());
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn working_calendar_saved_units_require_exact_dates_and_total() {
        let date: NaiveDate = "2026-10-03".parse().unwrap();
        let value = serde_json::json!([{"date":date,"units":"1"}]);
        assert_eq!(
            decode_leave_units(value.clone(), date, date, Decimal::ONE).unwrap(),
            vec![(date, Decimal::ONE)]
        );
        assert!(decode_leave_units(value.clone(), date, date, Decimal::from(2)).is_err());
        assert!(decode_leave_units(
            value,
            date.succ_opt().unwrap(),
            date.succ_opt().unwrap(),
            Decimal::ONE
        )
        .is_err());
        assert!(decode_leave_units(
            serde_json::json!([{"date":date,"units":"1"},{"date":date,"units":"1"}]),
            date,
            date,
            Decimal::from(2)
        )
        .is_err());
    }
}
