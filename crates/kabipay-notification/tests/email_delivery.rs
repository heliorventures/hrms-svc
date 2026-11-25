use chrono::{Duration, Utc};
use kabipay_common::important_email::EmailCategory;
use kabipay_notification::email::store::{retry_at, DeliveryCutoffs};
#[test]
fn safe_retries_are_bounded_by_five_retries_24_hours_and_expiry() {
    let now = Utc::now();
    assert!(retry_at(0, now, now + Duration::days(7), now).unwrap() > now);
    assert!(retry_at(4, now, now + Duration::days(7), now).is_some());
    assert!(retry_at(5, now, now + Duration::days(7), now).is_none());
    assert!(retry_at(0, now - Duration::days(1), now + Duration::days(7), now).is_none());
    assert!(retry_at(0, now, now + Duration::seconds(1), now).is_none());
}
#[test]
fn disable_and_category_cutoff_skip_old() {
    let now = Utc::now();
    let policy = DeliveryCutoffs::enabled(
        now - Duration::hours(1),
        [(EmailCategory::Leave, now - Duration::minutes(30))],
    );
    assert!(policy.admits(EmailCategory::Leave, now, now));
    assert!(!policy.admits(EmailCategory::Leave, now - Duration::hours(2), now));
    assert!(
        !policy.admits(EmailCategory::Leave, now, now - Duration::hours(2)),
        "both event time and DB created time are required"
    );
    assert!(!policy.admits(EmailCategory::Payslip, now, now));
    assert!(!DeliveryCutoffs::disabled().admits(EmailCategory::Leave, now, now));
}
