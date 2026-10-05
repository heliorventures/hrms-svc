use chrono::NaiveDate;
use kabipay_tax::domain::{TaxRegime, TaxSettings};
use kabipay_tax::services::tax_submission::selected_settings;
use kabipay_tax::services::tax_submission::selection_date;
use uuid::Uuid;

#[test]
fn self_service_uses_actual_current_date_for_midmonth_assignments() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
    assert_eq!(selection_date(2026, today).unwrap(), today);
}

fn setting(from: &str, until: Option<&str>, regime: &str, revision: i32) -> TaxSettings {
    TaxSettings { id: Uuid::new_v4(), employee_id: Uuid::new_v4(), revision,
        input: serde_json::from_value(serde_json::json!({"regime":regime,"method":"ANNUAL_PROJECTION","effective_from":from,"effective_until":until,"resident":true})).unwrap() }
}
#[test]
fn assignments_cover_current_midmonth_future_midyear_and_historical_exit() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
    let rows = vec![setting("2026-10-15", None, "NEW", 1)];
    assert_eq!(
        selected_settings(rows, 2026, today)
            .unwrap()
            .unwrap()
            .input
            .regime,
        TaxRegime::New
    );
    let rows = vec![
        setting("2027-10-15", None, "NEW", 1),
        setting("2027-10-15", None, "OLD", 2),
    ];
    assert_eq!(
        selected_settings(rows, 2027, today)
            .unwrap()
            .unwrap()
            .input
            .regime,
        TaxRegime::Old
    );
    let rows = vec![
        setting("2025-05-01", Some("2025-10-31"), "OLD", 1),
        setting("2026-10-01", None, "NEW", 2),
    ];
    assert_eq!(
        selected_settings(rows, 2025, today)
            .unwrap()
            .unwrap()
            .input
            .regime,
        TaxRegime::Old
    );
}

#[test]
fn expired_replacements_never_revive_older_unbounded_settings_in_another_year() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
    let rows = vec![
        setting("2026-04-01", None, "OLD", 1),
        setting("2027-01-01", Some("2027-03-31"), "NEW", 2),
    ];
    assert!(selected_settings(rows, 2027, today).unwrap().is_none());
    let rows = vec![
        setting("2024-04-01", None, "OLD", 1),
        setting("2025-01-01", None, "NEW", 2),
        setting("2025-01-01", Some("2025-03-31"), "NEW", 3),
    ];
    assert!(selected_settings(rows, 2025, today).unwrap().is_none());
}
#[test]
fn historic_context_search_reaches_the_end_of_the_financial_year() {
    let today = NaiveDate::from_ymd_opt(2026, 10, 20).unwrap();
    assert_eq!(
        selection_date(2025, today).unwrap(),
        NaiveDate::from_ymd_opt(2026, 3, 31).unwrap()
    );
}
