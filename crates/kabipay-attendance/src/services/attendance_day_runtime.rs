//! Attendance-only expiry and transaction locking; never manufacture a checkout.
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0010_time_shift_roster::attendance;
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Statement, TransactionTrait};
use uuid::Uuid;
use super::{attendance_day::{self, AttendanceDayWindow}, attendance_regularization_service::lock_employee_dates};

pub(crate) fn derive_expiry(
    row: &mut attendance::Model, window: &AttendanceDayWindow, now: DateTime<Utc>,
) -> bool {
    if row.work_date != window.work_date || now < window.ends_at
        || row.status.as_deref() != Some("OPEN")
        || row.check_out_at.is_some() || row.check_out_time.is_some()
    { return false; }
    row.status = Some("INCOMPLETE".into());
    row.regularization_status = Some("MISSED_PUNCH_OUT".into());
    true
}

/// Project effective status without changing revision or persisting any row.
pub(crate) async fn project_rows<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, clock: TenantBusinessClock,
    rows: &mut [attendance::Model], now: DateTime<Utc>,
) -> KabiPayResult<()> {
    let mut windows = std::collections::HashMap::new();
    for row in rows.iter_mut().filter(|r| r.status.as_deref() == Some("OPEN")) {
        if row.tenant_id != tenant_id { return Err(KabiPayError::Forbidden("attendance tenant mismatch".into())); }
        if !windows.contains_key(&row.work_date) {
            windows.insert(row.work_date, attendance_day::window_for_date(db, tenant_id, clock, row.work_date, now).await?);
        }
        if let Some(window) = windows.get(&row.work_date) { derive_expiry(row, window, now); }
    }
    Ok(())
}

/// Policy serialization precedes every employee lock. Resample after locks and
/// add any newly current date before making a punch decision.
pub(crate) async fn lock_current_window(
    txn: &DatabaseTransaction, tenant_id: Uuid, employee_id: Uuid,
    clock: TenantBusinessClock, now: &mut impl FnMut() -> DateTime<Utc>,
) -> KabiPayResult<(AttendanceDayWindow, DateTime<Utc>)> {
    attendance_day::lock_policy(txn, tenant_id).await?;
    let open = open_rows(txn, tenant_id, Some(employee_id)).all(txn).await?;
    let mut dates: Vec<NaiveDate> = open.iter().map(|r| r.work_date).collect();
    for _ in 0..3 {
        let candidate = attendance_day::current_window(txn, tenant_id, clock, now()).await?;
        dates.push(candidate.work_date);
        lock_employee_dates(txn, tenant_id, employee_id, &dates).await?;
        let locked_now = now();
        let current = attendance_day::current_window(txn, tenant_id, clock, locked_now).await?;
        if dates.contains(&current.work_date) {
            let frozen = attendance_day::ensure_window(txn, tenant_id, clock, current.work_date, locked_now).await?;
            return Ok((frozen, locked_now));
        }
        // All writers need our policy lock, so taking the extended sorted set
        // cannot invert ordering against another employee/day writer.
        dates.push(current.work_date);
    }
    Err(KabiPayError::Conflict("attendance day changed while acquiring locks; retry".into()))
}

fn open_rows<C: ConnectionTrait>(
    _db: &C, tenant_id: Uuid, employee_id: Option<Uuid>,
) -> sea_orm::Select<attendance::Entity> {
    let query = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::Status.eq("OPEN"))
        .filter(attendance::Column::CheckOutAt.is_null())
        .filter(attendance::Column::CheckOutTime.is_null());
    match employee_id { Some(id) => query.filter(attendance::Column::EmployeeId.eq(id)), None => query }
}

/// Called only after policy and employee/day locks; re-read rows under those locks.
pub(crate) async fn expire_employee(
    txn: &DatabaseTransaction, tenant_id: Uuid, employee_id: Uuid,
    clock: TenantBusinessClock, now: DateTime<Utc>,
) -> KabiPayResult<u64> {
    let rows = open_rows(txn, tenant_id, Some(employee_id)).all(txn).await?;
    let mut expired = 0;
    for mut row in rows {
        let prior_reason = row.regularization_status.clone();
        let window = attendance_day::window_for_date(txn, tenant_id, clock, row.work_date, now).await?;
        if !derive_expiry(&mut row, &window, now) { continue; }
        attendance_day::ensure_window(txn, tenant_id, clock, row.work_date, now).await?;
        let result = txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE attendance SET status = 'INCOMPLETE', regularization_status = 'MISSED_PUNCH_OUT', updated_at = $4 WHERE tenant_id = $1 AND employee_id = $2 AND id = $3 AND status = 'OPEN' AND check_out_at IS NULL AND check_out_time IS NULL",
            vec![tenant_id.into(), employee_id.into(), row.id.into(), now.into()],
        )).await?;
        if result.rows_affected() != 1 { continue; }
        txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO audit_log (id, tenant_id, entity_type, entity_id, action, before_state, after_state, created_at) VALUES ($1, $2, 'ATTENDANCE', $3, 'EXPIRE', $4, $5, $6)",
            vec![Uuid::new_v4().into(), tenant_id.into(), row.id.into(),
                serde_json::json!({"status":"OPEN","regularization_status":prior_reason}).into(),
                serde_json::json!({"status":"INCOMPLETE","reason":"MISSED_PUNCH_OUT","work_date":row.work_date,"ends_at":window.ends_at}).into(), now.into()],
        )).await?;
        expired += 1;
    }
    Ok(expired)
}

#[derive(Debug, Default)]
pub struct ExpirySweepResult { pub expired: u64, pub failed: u64 }

/// Bounded, retryable worker sweep. Caller must hold ATTENDANCE entitlement.
pub async fn sweep_expired_attendance(
    db: &DatabaseConnection, tenant_id: Uuid, clock: TenantBusinessClock, limit: u64,
) -> KabiPayResult<ExpirySweepResult> {
    sweep_with_clock(db, tenant_id, clock, limit, Utc::now).await
}

async fn sweep_with_clock(
    db: &DatabaseConnection, tenant_id: Uuid, clock: TenantBusinessClock, limit: u64,
    mut now: impl FnMut() -> DateTime<Utc>,
) -> KabiPayResult<ExpirySweepResult> {
    let candidates = open_rows(db, tenant_id, None)
        .order_by_asc(attendance::Column::WorkDate).order_by_asc(attendance::Column::Id)
        .limit(limit.clamp(1, 100)).all(db).await?;
    let mut result = ExpirySweepResult::default();
    for candidate in candidates {
        let txn = db.begin().await?;
        let attempt = async {
            attendance_day::lock_policy(&txn, tenant_id).await?;
            // Re-read all OPEN dates while serialized with punches/corrections.
            let rows = open_rows(&txn, tenant_id, Some(candidate.employee_id)).all(&txn).await?;
            let dates: Vec<_> = rows.iter().map(|r| r.work_date).collect();
            lock_employee_dates(&txn, tenant_id, candidate.employee_id, &dates).await?;
            expire_employee(&txn, tenant_id, candidate.employee_id, clock, now()).await
        }.await;
        match attempt {
            Ok(count) => { txn.commit().await?; result.expired += count; }
            Err(error) => {
                txn.rollback().await?;
                result.failed += 1;
                tracing::warn!(code = error.code(), "attendance expiry will retry");
            }
        }
    }
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    use uuid::Uuid;
    pub(super) fn utc(value: &str) -> DateTime<Utc> { value.parse().unwrap() }
    pub(super) fn window() -> AttendanceDayWindow {
        AttendanceDayWindow {
            work_date: "2026-09-11".parse().unwrap(),
            starts_at: utc("2026-09-10T23:30:00Z"),
            ends_at: utc("2026-09-11T23:30:00Z"),
            timezone: "Asia/Kolkata".into(), boundary_minutes: 300,
            policy_version_id: Uuid::from_u128(3),
        }
    }
    pub(super) fn row() -> attendance::Model {
        attendance::Model {
            id: Uuid::from_u128(1), tenant_id: Uuid::from_u128(2), employee_id: Uuid::from_u128(4),
            shift_id: None, work_date: "2026-09-11".parse().unwrap(),
            check_in_time: Some("23:00:00".parse().unwrap()), check_out_time: None,
            check_in_at: Some(utc("2026-09-11T17:30:00Z")), check_out_at: None,
            check_in_lat: None, check_in_lng: None, check_out_lat: None, check_out_lng: None,
            source: Some("WEB".into()), status: Some("OPEN".into()), regularization_status: None,
            biometric_ref: None, overtime_hours: None, late_minutes: None, early_exit_minutes: None,
            created_at: utc("2026-09-11T17:30:00Z"), updated_at: utc("2026-09-11T17:30:00Z"),
        }
    }
    #[test]
    fn attendance_day_expiry_at_exact_end_preserves_null_checkout_and_original_identity() {
        let mut row = row();
        assert!(!derive_expiry(&mut row, &window(), utc("2026-09-11T23:29:59Z")));
        assert!(derive_expiry(&mut row, &window(), utc("2026-09-11T23:30:00Z")));
        assert_eq!(row.status.as_deref(), Some("INCOMPLETE"));
        assert_eq!(row.regularization_status.as_deref(), Some("MISSED_PUNCH_OUT"));
        assert_eq!(row.id, Uuid::from_u128(1));
        assert!(row.check_out_time.is_none() && row.check_out_at.is_none());
        assert_eq!(row.updated_at, utc("2026-09-11T17:30:00Z"), "read projection must retain persisted revision");
        assert!(!derive_expiry(&mut row, &window(), utc("2026-09-12T10:00:00Z")));
    }
    #[test]
    fn attendance_day_expiry_never_overwrites_a_corrected_or_completed_record() {
        let mut row = row();
        row.status = Some("COMPLETE".into());
        row.check_out_at = Some(utc("2026-09-11T22:30:00Z"));
        row.check_out_time = Some("04:00:00".parse().unwrap());
        let before = row.clone();
        assert!(!derive_expiry(&mut row, &window(), utc("2026-09-12T10:00:00Z")));
        assert_eq!(row, before);
    }
}

#[cfg(test)]
#[path = "attendance_day_runtime_tests.rs"]
mod integration_tests;
