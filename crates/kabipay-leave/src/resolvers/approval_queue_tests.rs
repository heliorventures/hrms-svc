use super::*;
use sea_orm::{DbBackend, QueryTrait};

#[test]
fn queue_filters_before_pagination_beyond_120_and_counts_full_scope() {
    let mut queue = QueuePage::new(2, 1);
    // The first matching request is older than the former 120-row cap.
    for id in 0..130 {
        queue.consider(id, "APPROVED", false, Some("PENDING"), true);
    }
    queue.consider(130, "PENDING", false, Some("PENDING"), true);
    for id in 131..135 {
        queue.consider(id, "PENDING", true, Some("PENDING"), true);
    }
    assert_eq!(queue.rows, vec![132, 133]);
    assert_eq!(queue.total_count, 4);
    assert_eq!(queue.pending_count, 5);
    assert_eq!(queue.actionable_count, 4);
}

#[test]
fn queue_counts_are_independent_of_status_and_out_of_range_pages() {
    let mut queue = QueuePage::new(20, 99);
    queue.consider(1, "PENDING", false, Some("APPROVED"), false);
    queue.consider(2, "PENDING", true, Some("APPROVED"), false);
    queue.consider(3, "APPROVED", false, Some("APPROVED"), false);
    assert!(queue.rows.is_empty());
    assert_eq!((queue.total_count, queue.pending_count, queue.actionable_count), (1, 2, 1));
}

#[test]
fn queue_sql_binds_tenant_deletion_overlap_scope_and_stable_cursor() {
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let cursor_id = Uuid::new_v4();
    let date = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
    let filter = EmployeeScopeFilter::EmployeeIds(vec![employee]);
    let sql = candidate_query(tenant, &filter, Some(date), Some(date), Some((date.and_hms_opt(0, 0, 0).unwrap().and_utc(), cursor_id)))
        .build(DbBackend::Postgres).to_string();
    for expected in [tenant.to_string(), employee.to_string(), cursor_id.to_string(), "is_deleted".into(), "to_date".into(), "from_date".into(), "ORDER BY".into(), "LIMIT 200".into()] {
        assert!(sql.contains(&expected), "missing {expected}: {sql}");
    }
    let empty_sql = candidate_query(tenant, &EmployeeScopeFilter::Empty, None, None, None)
        .build(DbBackend::Postgres).to_string();
    assert!(empty_sql.contains("1 = 2"), "empty scope must fail closed: {empty_sql}");
}

#[test]
fn queue_rejects_invalid_filters_before_database_access() {
    assert!(validate_filters(0, None, None, None).is_err());
    assert!(validate_filters(201, None, None, None).is_err());
    assert!(validate_filters(20, None, None, Some("UNKNOWN")).is_err());
    let from = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
    let to = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    assert!(validate_filters(20, Some(from), Some(to), None).is_err());
    assert!(validate_filters(200, None, None, Some("PENDING")).is_ok());
}
