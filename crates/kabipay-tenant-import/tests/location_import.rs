use chrono::NaiveDate;
use kabipay_tenant_import::location_import::{needs_assignment, AssignmentState, LocationInput};
use uuid::Uuid;

fn date(value: &str) -> NaiveDate {
    value.parse().unwrap()
}

fn state() -> AssignmentState {
    AssignmentState {
        current: None,
        latest: None,
        joining: date("2020-01-01"),
        calendar_active: false,
    }
}

#[test]
fn initial_reviewed_historical_location_is_supported_before_activation() {
    assert!(needs_assignment(
        &state(),
        Uuid::new_v4(),
        date("2026-09-01"),
        date("2026-10-07")
    )
    .unwrap());
}

#[test]
fn exact_replay_is_unchanged_even_after_activation() {
    let id = Uuid::new_v4();
    let state = AssignmentState {
        current: Some(id),
        latest: Some((Some(id), date("2026-09-01"))),
        calendar_active: true,
        ..state()
    };
    assert!(!needs_assignment(&state, id, date("2026-09-01"), date("2026-10-07")).unwrap());
}

#[test]
fn newer_history_cannot_be_overwritten_by_an_old_import() {
    let id = Uuid::new_v4();
    let state = AssignmentState {
        current: Some(id),
        latest: Some((Some(id), date("2026-10-07"))),
        ..state()
    };
    assert_eq!(
        needs_assignment(&state, id, date("2026-09-01"), date("2026-10-07"))
            .unwrap_err()
            .to_string(),
        "LOCATION_ASSIGNMENT_HISTORY_REVIEW_REQUIRED"
    );
}

#[test]
fn legacy_location_is_preserved_unless_it_matches_the_reviewed_assignment() {
    let id = Uuid::new_v4();
    let state = AssignmentState {
        current: Some(id),
        ..state()
    };
    assert!(needs_assignment(&state, id, date("2026-10-07"), date("2026-10-07")).unwrap());
    assert!(needs_assignment(
        &state,
        Uuid::new_v4(),
        date("2026-10-07"),
        date("2026-10-07")
    )
    .is_err());
}

#[test]
fn active_calendar_requires_today_for_new_assignments() {
    let state = AssignmentState {
        calendar_active: true,
        ..state()
    };
    let id = Uuid::new_v4();
    assert!(needs_assignment(&state, id, date("2026-10-07"), date("2026-10-07")).unwrap());
    assert!(needs_assignment(&state, id, date("2026-10-06"), date("2026-10-07")).is_err());
}

#[test]
fn future_and_prejoining_dates_are_rejected() {
    for effective in [date("2019-12-31"), date("2026-10-08")] {
        assert!(needs_assignment(&state(), Uuid::new_v4(), effective, date("2026-10-07")).is_err());
    }
}

#[test]
fn inconsistent_current_link_and_history_require_review() {
    let id = Uuid::new_v4();
    let state = AssignmentState {
        latest: Some((Some(id), date("2026-10-07"))),
        ..state()
    };
    assert!(needs_assignment(&state, id, date("2026-10-07"), date("2026-10-07")).is_err());
}

#[test]
fn names_share_ui_normalization_and_reject_controls() {
    let input = LocationInput {
        name: "  Pune   Office ".into(),
        effective_from: date("2026-10-07"),
    };
    assert_eq!(input.normalized_name().unwrap(), "Pune Office");
    for name in [" ", "Office\nPune"] {
        assert!(LocationInput {
            name: name.into(),
            ..input.clone()
        }
        .normalized_name()
        .is_err());
    }
}
