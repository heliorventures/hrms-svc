use super::*;
use chrono::{DateTime, NaiveDate, Utc};
use uuid::Uuid;
fn date(s: &str) -> NaiveDate { s.parse().unwrap() }
fn utc(s: &str) -> DateTime<Utc> { s.parse().unwrap() }
fn version(effective: &str, minutes: i32, timezone: &str) -> PolicyVersion {
    PolicyVersion { id: Uuid::new_v4(), effective_work_date: date(effective), boundary_minutes: minutes, timezone: timezone.into() }
}
#[test]
fn attendance_day_boundary_validation() {
    assert_eq!(parse_boundary_minutes("05:00").unwrap(), 300);
    assert_eq!(parse_boundary_minutes("23:59").unwrap(), 1439);
    for bad in ["24:00", "5:00", "05:60", "-1:00", "05:00:00", " 05:00", "aa:bb"] { assert!(parse_boundary_minutes(bad).is_err()); }
}
#[test]
fn attendance_day_literal_kolkata_boundary() {
    let policies = [version("0001-01-01", 300, "Asia/Kolkata")];
    let w = resolve_window(&policies, date("2026-09-11")).unwrap();
    assert_eq!(w.starts_at, utc("2026-09-10T23:30:00Z"));
    assert_eq!(w.ends_at, utc("2026-09-11T23:30:00Z"));
    for (instant, expected) in [("2026-09-11T23:29:59Z", "2026-09-11"), ("2026-09-11T23:30:00Z", "2026-09-12"), ("2026-12-31T23:30:00Z", "2027-01-01")] {
        let now = utc(instant);
        let current = resolve_current_window(&policies, now).unwrap();
        assert_eq!(current.work_date, date(expected));
        assert!(current.starts_at <= now && now < current.ends_at);
    }
}
#[test]
fn attendance_day_transition_inherits_start_and_preserves_active_day() {
    for (minutes, end, hours) in [(360, "2026-09-13T00:30:00Z", 25), (240, "2026-09-12T22:30:00Z", 23)] {
        let baseline = version("0001-01-01", 300, "Asia/Kolkata");
        let active = resolve_window(&[baseline.clone()], date("2026-09-11")).unwrap();
        let policies = [baseline, version("2026-09-12", minutes, "Asia/Kolkata")];
        assert_eq!(resolve_window(&policies, date("2026-09-11")).unwrap(), active);
        let transition = resolve_window(&policies, date("2026-09-12")).unwrap();
        assert_eq!(transition.starts_at, utc("2026-09-11T23:30:00Z"));
        assert_eq!(transition.ends_at, utc(end));
        assert_eq!((transition.ends_at - transition.starts_at).num_hours(), hours);
        assert_eq!(active.ends_at, transition.starts_at);
        assert_eq!(transition.ends_at, resolve_window(&policies, date("2026-09-13")).unwrap().starts_at);
    }
}
#[test]
fn attendance_day_dst_resolves_gap_and_overlap_deterministically() {
    for (minutes, day, start, end) in [
        (150, "2026-03-08", "2026-03-08T07:00:00Z", "2026-03-09T06:30:00Z"),
        (90, "2026-11-01", "2026-11-01T05:30:00Z", "2026-11-02T06:30:00Z")
    ] {
        let policies = [version("0001-01-01", minutes, "America/New_York")];
        let w = resolve_window(&policies, date(day)).unwrap();
        assert_eq!(w.starts_at, utc(start)); assert_eq!(w.ends_at, utc(end));
    }
}
#[test]
fn attendance_day_rejects_overflow_invalid_policy_and_nonincreasing_skipped_date() {
    assert!(resolve_window(&[], date("2026-09-11")).is_err());
    assert!(resolve_window(&[version("0001-01-01", 1440, "UTC")], date("2026-09-11")).is_err());
    assert!(resolve_window(&[version("0001-01-01", 0, "UTC")], NaiveDate::MAX).is_err());
    assert!(resolve_window(&[version("0001-01-01", 0, "Pacific/Apia")], date("2011-12-30")).is_err());
}
#[test]
fn attendance_day_midnight_custom_and_month_boundary_literals() {
    for (minutes, day, start, end) in [
        (0, "2026-10-01", "2026-09-30T18:30:00Z", "2026-10-01T18:30:00Z"),
        (345, "2026-10-01", "2026-10-01T00:15:00Z", "2026-10-02T00:15:00Z"),
        (1439, "2026-12-31", "2026-12-31T18:29:00Z", "2027-01-01T18:29:00Z")
    ] {
        let w = resolve_window(&[version("0001-01-01", minutes, "Asia/Kolkata")], date(day)).unwrap();
        assert_eq!(w.starts_at, utc(start)); assert_eq!(w.ends_at, utc(end));
    }
}
#[test]
fn attendance_day_timezone_transition_keeps_predecessor_endpoint() {
    let versions = [version("0001-01-01", 300, "Asia/Kolkata"), version("2026-09-12", 300, "America/New_York")];
    let w = resolve_window(&versions, date("2026-09-12")).unwrap();
    assert_eq!(w.starts_at, utc("2026-09-11T23:30:00Z"));
    assert_eq!(w.ends_at, utc("2026-09-13T09:00:00Z"));
    let current = resolve_current_window(&versions, utc("2026-09-12T00:00:00Z")).unwrap();
    assert_eq!(current.work_date, date("2026-09-12"));
}
