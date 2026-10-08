//! Shared timestamp interpretation for attendance totals and reports.

use chrono::{DateTime, Days, Utc};
use kabipay_common::tenant_business_clock::TenantBusinessClock;
use kabipay_db_entities::tenant::d0010_time_shift_roster::attendance;

pub(super) fn canonical_instants(
    row: &attendance::Model,
    clock: TenantBusinessClock,
) -> (Option<DateTime<Utc>>, Option<DateTime<Utc>>) {
    let legacy_check_in = row
        .check_in_time
        .and_then(|time| clock.to_utc(row.work_date, time).ok());
    let legacy_check_out = match (row.check_in_time, row.check_out_time) {
        (Some(check_in), Some(check_out)) if check_out != check_in => {
            let checkout_date = if check_out > check_in {
                Some(row.work_date)
            } else {
                row.work_date.checked_add_days(Days::new(1))
            };
            checkout_date.and_then(|date| clock.to_utc(date, check_out).ok())
        }
        _ => None,
    };
    (
        row.check_in_at.or(legacy_check_in),
        row.check_out_at.or(legacy_check_out),
    )
}
