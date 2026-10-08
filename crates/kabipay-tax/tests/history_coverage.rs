use chrono::NaiveDate;
use kabipay_tax::services::tax_history::require_no_overlap;
fn date(s: &str) -> NaiveDate {
    s.parse().unwrap()
}
#[test]
fn september_actual_cannot_be_counted_twice_in_opening_history() {
    assert!(require_no_overlap(
        date("2026-04-01"),
        date("2026-09-30"),
        date("2026-09-01"),
        date("2026-09-30")
    )
    .is_err());
    assert!(require_no_overlap(
        date("2026-04-01"),
        date("2026-08-31"),
        date("2026-09-01"),
        date("2026-09-30")
    )
    .is_ok());
}
