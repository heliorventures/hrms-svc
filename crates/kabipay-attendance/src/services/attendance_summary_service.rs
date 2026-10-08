//! Complete-period self-service totals, independent of attendance cursor pages.

use std::collections::HashSet;

use chrono::NaiveDate;
use kabipay_common::{tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0010_time_shift_roster::attendance;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

use super::{
    attendance_duration::canonical_instants, attendance_management_service::validate_date_range,
};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct AttendancePeriodSummary {
    pub completed_minutes: i32,
    pub worked_days: i32,
    pub average_minutes: Option<f64>,
    pub incomplete_segments: i32,
}

fn summarize(
    rows: &[attendance::Model],
    clock: TenantBusinessClock,
    from_date: NaiveDate,
    to_date: NaiveDate,
) -> KabiPayResult<AttendancePeriodSummary> {
    validate_date_range(from_date, to_date)?;
    let overflow = || KabiPayError::Internal("attendance summary exceeds supported range".into());
    let mut seconds = 0i64;
    let mut worked_dates = HashSet::new();
    let mut incomplete_segments = 0i32;
    for row in rows {
        if row.work_date < from_date || row.work_date > to_date {
            continue;
        }
        let (start, end) = canonical_instants(row, clock);
        match (start, end) {
            (Some(start), Some(end)) if end > start => {
                let duration = end.signed_duration_since(start).num_seconds();
                if duration > 0 {
                    seconds = seconds.checked_add(duration).ok_or_else(overflow)?;
                    worked_dates.insert(row.work_date);
                }
            }
            (Some(_), None) => {
                incomplete_segments = incomplete_segments.checked_add(1).ok_or_else(overflow)?;
            }
            _ => {}
        }
    }
    let worked_days = i32::try_from(worked_dates.len()).map_err(|_| overflow())?;
    let completed_minutes = i32::try_from(seconds.checked_add(30).ok_or_else(overflow)? / 60)
        .map_err(|_| overflow())?;
    Ok(AttendancePeriodSummary {
        completed_minutes,
        worked_days,
        average_minutes: (worked_days > 0).then(|| seconds as f64 / 60.0 / f64::from(worked_days)),
        incomplete_segments,
    })
}

pub async fn my_attendance_summary(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    from_date: NaiveDate,
    to_date: NaiveDate,
    clock: TenantBusinessClock,
) -> KabiPayResult<AttendancePeriodSummary> {
    validate_date_range(from_date, to_date)?;
    // The filter is the workdate, never the checkout instant: overnight work stays
    // in the starting day/month. Do not apply a pagination limit to this query.
    let rows = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.gte(from_date))
        .filter(attendance::Column::WorkDate.lte(to_date))
        .all(db)
        .await?;
    summarize(&rows, clock, from_date, to_date)
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{DateTime, Utc};
    use uuid::Uuid;

    fn segment(date: &str, start: &str, end: Option<&str>) -> attendance::Model {
        let check_in_at: DateTime<Utc> = start.parse().unwrap();
        attendance::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::nil(),
            employee_id: Uuid::from_u128(42),
            shift_id: None,
            work_date: date.parse().unwrap(),
            check_in_at: Some(check_in_at),
            check_out_at: end.map(|value| value.parse().unwrap()),
            check_in_time: None,
            check_out_time: None,
            check_in_lat: None,
            check_in_lng: None,
            check_out_lat: None,
            check_out_lng: None,
            source: Some("SELF_SERVICE".into()),
            status: None,
            regularization_status: None,
            biometric_ref: None,
            overtime_hours: None,
            late_minutes: None,
            early_exit_minutes: None,
            created_at: check_in_at,
            updated_at: check_in_at,
        }
    }

    fn september(rows: &[attendance::Model]) -> AttendancePeriodSummary {
        summarize(
            rows,
            TenantBusinessClock::from_name("Asia/Kolkata").unwrap(),
            "2026-09-01".parse().unwrap(),
            "2026-09-30".parse().unwrap(),
        ).unwrap()
    }

    #[test]
    fn completed_segments_count_even_when_the_same_day_has_an_open_punch() {
        let result = september(&[
            segment("2026-09-01", "2026-09-01T03:30:00Z", Some("2026-09-01T07:30:00Z")),
            segment("2026-09-01", "2026-09-01T08:30:00Z", None),
            segment("2026-09-02", "2026-09-02T03:30:00Z", None),
            segment("2026-09-03", "2026-09-03T03:30:00Z", Some("2026-09-03T09:30:00Z")),
        ]);
        assert_eq!(result.completed_minutes, 600);
        assert_eq!(result.worked_days, 2);
        assert_eq!(result.average_minutes, Some(300.0));
        assert_eq!(result.incomplete_segments, 2);
    }

    #[test]
    fn summary_includes_more_than_one_page_and_counts_each_workdate_once() {
        let mut rows = Vec::new();
        for day in 1..=30 {
            let date = format!("2026-09-{day:02}");
            rows.push(segment(&date, &format!("{date}T03:30:00Z"), Some(&format!("{date}T07:30:00Z"))));
            rows.push(segment(&date, &format!("{date}T08:30:00Z"), Some(&format!("{date}T12:30:00Z"))));
        }
        let result = september(&rows);
        assert_eq!(result.completed_minutes, 30 * 480);
        assert_eq!(result.worked_days, 30);
        assert_eq!(result.average_minutes, Some(480.0));
    }

    #[test]
    fn overnight_work_stays_with_the_original_workdate_across_month_boundaries() {
        let result = september(&[
            segment("2026-08-31", "2026-08-31T16:30:00Z", Some("2026-09-01T01:30:00Z")),
            segment("2026-09-30", "2026-09-30T16:30:00Z", Some("2026-10-01T01:30:00Z")),
        ]);
        assert_eq!(result.completed_minutes, 540);
        assert_eq!(result.worked_days, 1);
    }

    #[test]
    fn canonical_instants_take_precedence_and_legacy_overnight_rows_remain_supported() {
        let mut canonical = segment("2026-09-01", "2026-09-01T03:30:00Z", Some("2026-09-01T07:30:00Z"));
        canonical.check_in_time = Some("01:00:00".parse().unwrap());
        canonical.check_out_time = Some("02:00:00".parse().unwrap());
        let mut legacy = segment("2026-09-30", "2026-09-30T16:30:00Z", None);
        legacy.check_in_at = None;
        legacy.check_in_time = Some("22:00:00".parse().unwrap());
        legacy.check_out_time = Some("07:00:00".parse().unwrap());
        let result = september(&[canonical, legacy]);
        assert_eq!(result.completed_minutes, 780);
        assert_eq!(result.average_minutes, Some(390.0));
    }

    #[test]
    fn invalid_and_open_only_rows_do_not_create_an_average() {
        let result = september(&[
            segment("2026-09-01", "2026-09-01T03:30:00Z", None),
            segment("2026-09-02", "2026-09-02T03:30:00Z", Some("2026-09-02T03:30:00Z")),
            segment("2026-09-03", "2026-09-03T07:30:00Z", Some("2026-09-03T03:30:00Z")),
        ]);
        assert_eq!(result.worked_days, 0);
        assert_eq!(result.completed_minutes, 0);
        assert_eq!(result.average_minutes, None);
        assert_eq!(result.incomplete_segments, 1);
        assert_eq!(september(&[]), AttendancePeriodSummary::default());
    }

    #[test]
    fn durations_are_rounded_after_aggregation_not_per_segment() {
        let result = september(&[
            segment("2026-09-01", "2026-09-01T03:30:00Z", Some("2026-09-01T03:30:20Z")),
            segment("2026-09-01", "2026-09-01T03:31:00Z", Some("2026-09-01T03:31:20Z")),
            segment("2026-09-01", "2026-09-01T03:32:00Z", Some("2026-09-01T03:32:20Z")),
        ]);
        assert_eq!(result.completed_minutes, 1);
        assert_eq!(result.average_minutes, Some(1.0));
        assert_eq!(result.worked_days, 1);
    }
}
