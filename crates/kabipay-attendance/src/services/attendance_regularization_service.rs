//! Transaction-scoped attendance adjustment validation and audit writes.

use chrono::{DateTime, NaiveDate, NaiveTime, Timelike, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_common::tenant_business_clock::TenantBusinessClock;
use kabipay_db_entities::tenant::{
    d0010_time_shift_roster::attendance,
    d0063_attendance_management::attendance_adjustment_audit,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait,
    QueryFilter, QueryOrder, Set, Statement,
};
use serde::Serialize;
use serde_json::Value;
use uuid::Uuid;

use crate::services::hrms_master_service;

pub(crate) const ATTENDANCE_STATUS_COMPLETE: &str = "COMPLETE";
pub(crate) const MANUAL_ATTENDANCE_SOURCE: &str = "WEB+MANUAL";
pub(crate) const MANUAL_SELF_REPORTED: &str = "SELF_REPORTED";
const MANUAL_REGULARIZED: &str = "REGULARIZED";
const MAX_DAY_MINUTES: i32 = 24 * 60;
const ATTENDANCE_MANAGEMENT_ACCESS_DENIED: &str = "attendance management access denied";

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentTimes {
    pub work_date: NaiveDate,
    pub check_in_time: NaiveTime,
    pub check_out_time: NaiveTime,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct SegmentInstants {
    pub check_in_at: DateTime<Utc>,
    pub check_out_at: DateTime<Utc>,
}

impl SegmentTimes {
    pub fn to_window_instants(
        self, window: &crate::services::attendance_day::AttendanceDayWindow,
        actual_dates: Option<(NaiveDate, NaiveDate)>,
    ) -> KabiPayResult<SegmentInstants> {
        let clock = TenantBusinessClock::from_name(&window.timezone)?;
        let contained = |instants: SegmentInstants| {
            instants.check_in_at >= window.starts_at && instants.check_in_at < window.ends_at
                && instants.check_out_at > instants.check_in_at && instants.check_out_at <= window.ends_at
        };
        if let Some((check_in_date, check_out_date)) = actual_dates {
            let instants = SegmentInstants {
                check_in_at: clock.to_utc(check_in_date, self.check_in_time)?,
                check_out_at: clock.to_utc(check_out_date, self.check_out_time)?,
            };
            return if contained(instants) { Ok(instants) } else {
                Err(KabiPayError::Validation("attendance interval must be contained in its attendance day window".into()))
            };
        }
        // With no actual dates, retain only an unambiguous interval. Transition
        // windows can contain the same wall time on two dates; never guess.
        let start_date = clock.business_date(window.starts_at);
        let end_date = clock.business_date(window.ends_at);
        let mut dates = vec![start_date];
        let mut date = start_date;
        while date < end_date {
            date = date.succ_opt().ok_or_else(|| KabiPayError::Validation("attendance date overflow".into()))?;
            dates.push(date);
        }
        let mut candidates = Vec::new();
        for start in &dates {
            for end in &dates {
                if let (Ok(check_in_at), Ok(check_out_at)) = (clock.to_utc(*start, self.check_in_time), clock.to_utc(*end, self.check_out_time)) {
                    let instants = SegmentInstants { check_in_at, check_out_at };
                    if contained(instants) { candidates.push(instants); }
                }
            }
        }
        if candidates.len() != 1 {
            return Err(KabiPayError::Validation("provide actual check-in and checkout dates for an unambiguous interval inside the attendance window".into()));
        }
        Ok(candidates[0])
    }
    /// Builds an employee-entered or HR-entered segment using the precision exposed by the UI.
    /// Live punch instants remain second-precise; manual boundaries are intentionally minute-precise.
    pub fn for_manual_input(
        work_date: NaiveDate,
        check_in_time: NaiveTime,
        check_out_time: NaiveTime,
    ) -> Self {
        Self {
            work_date,
            check_in_time: truncate_to_minute(check_in_time),
            check_out_time: truncate_to_minute(check_out_time),
        }
    }

    pub fn to_instants(self, clock: TenantBusinessClock) -> KabiPayResult<SegmentInstants> {
        let check_in_at = clock.to_utc(self.work_date, self.check_in_time)?;
        let check_out_at = clock.to_utc(self.work_date, self.check_out_time)?;
        if check_out_at <= check_in_at {
            return Err(KabiPayError::Validation(
                "checkInTime must be before checkOutTime (same-day segment only)".into(),
            ));
        }
        Ok(SegmentInstants {
            check_in_at,
            check_out_at,
        })
    }
}

fn truncate_to_minute(time: NaiveTime) -> NaiveTime {
    NaiveTime::from_hms_opt(time.hour(), time.minute(), 0)
        .expect("hour and minute from NaiveTime are always valid")
}

#[derive(Clone, Debug)]
pub struct ManagedCreateCommand {
    pub tenant_id: Uuid,
    pub target_employee_id: Uuid,
    pub actor_user_id: Uuid,
    pub segment: SegmentTimes,
    pub clock: TenantBusinessClock,
    pub actual_dates: Option<(NaiveDate, NaiveDate)>,
    pub reason: String,
    pub request_id: Option<String>,
}

#[derive(Clone, Debug)]
pub(crate) struct ManagedUpdateCommand {
    pub tenant_id: Uuid,
    pub attendance_id: Uuid,
    pub target_employee_id: Uuid,
    pub actor_user_id: Uuid,
    pub initial_work_date: NaiveDate,
    pub segment: SegmentTimes,
    pub clock: TenantBusinessClock,
    pub actual_dates: Option<(NaiveDate, NaiveDate)>,
    pub reason: String,
    pub request_id: Option<String>,
    pub expected_updated_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Eq, PartialEq)]
enum AttendanceAuditOperation {
    Create,
    Update,
}

impl AttendanceAuditOperation {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Create => "CREATE",
            Self::Update => "UPDATE",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
struct AttendanceAuditInsert {
    tenant_id: Uuid,
    attendance_id: Uuid,
    target_employee_id: Uuid,
    actor_user_id: Uuid,
    operation: AttendanceAuditOperation,
    reason: String,
    before_values: Option<Value>,
    after_values: Value,
    request_id: Option<String>,
    created_at: DateTime<Utc>,
}

#[derive(Clone, Debug, Serialize)]
pub struct AttendanceAuditSnapshot {
    pub work_date: NaiveDate,
    pub check_in_time: NaiveTime,
    pub check_out_time: Option<NaiveTime>,
    pub check_in_at: Option<DateTime<Utc>>,
    pub check_out_at: Option<DateTime<Utc>>,
    pub status: String,
    pub source: String,
    pub regularization_status: Option<String>,
    pub updated_at: DateTime<Utc>,
}

impl TryFrom<&attendance::Model> for AttendanceAuditSnapshot {
    type Error = KabiPayError;

    fn try_from(row: &attendance::Model) -> Result<Self, Self::Error> {
        Ok(Self {
            work_date: row.work_date,
            check_in_time: row.check_in_time.ok_or_else(|| {
                KabiPayError::Validation("attendance segment has no check-in time".into())
            })?,
            check_out_time: row.check_out_time,
            check_in_at: row.check_in_at,
            check_out_at: row.check_out_at,
            status: row.status.clone().ok_or_else(|| {
                KabiPayError::Validation("attendance segment has no status".into())
            })?,
            source: row.source.clone().ok_or_else(|| {
                KabiPayError::Validation("attendance segment has no source".into())
            })?,
            regularization_status: row.regularization_status.clone(),
            updated_at: row.updated_at,
        })
    }
}

fn validate_reason(reason: &str) -> KabiPayResult<String> {
    let trimmed = reason.trim();
    if !(5..=500).contains(&trimmed.chars().count()) {
        return Err(KabiPayError::Validation(
            "reason must be between 5 and 500 characters".into(),
        ));
    }
    Ok(trimmed.to_owned())
}

fn lock_dates(old_date: NaiveDate, new_date: NaiveDate) -> Vec<NaiveDate> {
    let mut dates = vec![old_date, new_date];
    dates.sort_unstable();
    dates.dedup();
    dates
}

pub(crate) fn assert_locked_attendance_identity(
    locked_employee_id: Uuid,
    locked_work_date: NaiveDate,
    current_employee_id: Uuid,
    current_work_date: NaiveDate,
) -> KabiPayResult<()> {
    if current_employee_id != locked_employee_id || current_work_date != locked_work_date {
        return Err(KabiPayError::Conflict(
            "attendance segment changed while acquiring locks; refresh before retrying".into(),
        ));
    }
    Ok(())
}

pub(crate) fn assert_total_attendance_minutes_under_daily_cap(
    total_minutes: i32,
) -> KabiPayResult<()> {
    if total_minutes >= MAX_DAY_MINUTES {
        return Err(KabiPayError::Validation(
            "total attendance for a day must be less than 24 hours".into(),
        ));
    }
    Ok(())
}

#[cfg(test)]
fn validate_segment_against_rows(
    segment: SegmentTimes, today: NaiveDate, max_self_adjust_days: i64,
    existing: &[attendance::Model], excluded_attendance_id: Option<Uuid>,
    bypass_self_service_age_window: bool,
) -> KabiPayResult<()> {
    let clock = TenantBusinessClock::from_name("UTC")?;
    let instants = segment.to_instants(clock)?;
    validate_resolved_segment_against_rows(segment, instants, clock, today,
        max_self_adjust_days, existing, excluded_attendance_id, bypass_self_service_age_window)
}

pub(crate) async fn validate_segment_with_connection(
    db: &DatabaseTransaction,
    tenant_id: Uuid,
    employee_id: Uuid,
    segment: SegmentTimes,
    excluded_attendance_id: Option<Uuid>,
    bypass_self_service_age_window: bool,
    clock: TenantBusinessClock,
    actual_dates: Option<(NaiveDate, NaiveDate)>,
    now: DateTime<Utc>,
) -> KabiPayResult<SegmentInstants> {
    let window = super::attendance_day::ensure_window(db, tenant_id, clock, segment.work_date, now).await?;
    if window.starts_at > now { return Err(KabiPayError::Validation("workDate cannot be in the future".into())); }
    let instants = segment.to_window_instants(&window, actual_dates)?;
    let policy = hrms_master_service::load_attendance_adjustment_policy(db, tenant_id).await?;
    let existing = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.eq(segment.work_date))
        .order_by_asc(attendance::Column::CreatedAt)
        .all(db)
        .await?;
    validate_resolved_segment_against_rows(
        segment, instants, TenantBusinessClock::from_name(&window.timezone)?,
        clock.business_date(now),
        policy.max_self_adjust_days,
        &existing,
        excluded_attendance_id,
        bypass_self_service_age_window,
    )?;
    Ok(instants)
}

fn validate_resolved_segment_against_rows(
    segment: SegmentTimes, instants: SegmentInstants, clock: TenantBusinessClock,
    today: NaiveDate, max_self_adjust_days: i64, existing: &[attendance::Model],
    excluded_attendance_id: Option<Uuid>, bypass_self_service_age_window: bool,
) -> KabiPayResult<()> {
    if segment.work_date > today { return Err(KabiPayError::Validation("workDate cannot be in the future".into())); }
    if today.signed_duration_since(segment.work_date).num_days() > max_self_adjust_days.max(0)
        && !bypass_self_service_age_window {
        return Err(KabiPayError::Forbidden(format!("manual attendance is limited to the last {} calendar days unless you hold attendance regularization permission", max_self_adjust_days.max(0))));
    }
    let mut seconds = instants.check_out_at.signed_duration_since(instants.check_in_at).num_seconds();
    if seconds <= 0 { return Err(KabiPayError::Validation("check-in must precede checkout".into())); }
    for row in existing.iter().filter(|row| Some(row.id) != excluded_attendance_id) {
        let mut normalized = row.clone();
        if row.source.as_deref() == Some(MANUAL_ATTENDANCE_SOURCE) && row.check_in_at.is_none() && row.check_out_at.is_none() {
            normalized.check_in_time = row.check_in_time.map(truncate_to_minute);
            normalized.check_out_time = row.check_out_time.map(truncate_to_minute);
        }
        match super::attendance_duration::canonical_instants(&normalized, clock) {
            (Some(start), Some(end)) if end > start => {
                if instants.check_in_at < end && instants.check_out_at > start {
                    return Err(KabiPayError::Validation("manual attendance overlaps with an existing segment for this day".into()));
                }
                seconds = seconds.checked_add(end.signed_duration_since(start).num_seconds())
                    .ok_or_else(|| KabiPayError::Internal("attendance daily duration overflow".into()))?;
            }
            (Some(_), None) => return Err(KabiPayError::Validation("correct the original incomplete or open punch before adding attendance for this day".into())),
            _ => {}
        }
    }
    assert_total_attendance_minutes_under_daily_cap(i32::try_from(seconds / 60)
        .map_err(|_| KabiPayError::Internal("attendance daily duration overflow".into()))?)
}

/// Acquires transaction-scoped locks for one employee's dates in stable order.
pub async fn lock_employee_dates(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    employee_id: Uuid,
    dates: &[NaiveDate],
) -> KabiPayResult<()> {
    super::attendance_day::lock_policy(txn, tenant_id).await?;
    let mut ordered_dates = dates.to_vec();
    ordered_dates.sort_unstable();
    ordered_dates.dedup();
    for work_date in ordered_dates {
        let key = format!("attendance:{tenant_id}:{employee_id}:{work_date}");
        txn.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
            vec![key.into()],
        ))
        .await?;
    }
    Ok(())
}

pub(crate) async fn insert_manual_segment<C>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    segment: SegmentTimes,
    instants: SegmentInstants,
    regularization_status: &'static str,
    now: DateTime<Utc>,
) -> KabiPayResult<attendance::Model>
where
    C: ConnectionTrait,
{
    attendance::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        employee_id: Set(employee_id),
        shift_id: Set(None),
        work_date: Set(segment.work_date),
        check_in_time: Set(Some(segment.check_in_time)),
        check_out_time: Set(Some(segment.check_out_time)),
        check_in_at: Set(Some(instants.check_in_at)),
        check_out_at: Set(Some(instants.check_out_at)),
        check_in_lat: Set(None),
        check_in_lng: Set(None),
        check_out_lat: Set(None),
        check_out_lng: Set(None),
        source: Set(Some(MANUAL_ATTENDANCE_SOURCE.into())),
        status: Set(Some(ATTENDANCE_STATUS_COMPLETE.into())),
        regularization_status: Set(Some(regularization_status.into())),
        biometric_ref: Set(None),
        overtime_hours: Set(None),
        late_minutes: Set(None),
        early_exit_minutes: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await
    .map_err(KabiPayError::from)
}

pub(crate) async fn update_manual_segment<C>(
    db: &C,
    row: attendance::Model,
    segment: SegmentTimes,
    instants: SegmentInstants,
    regularization_status: &'static str,
    now: DateTime<Utc>,
) -> KabiPayResult<attendance::Model>
where
    C: ConnectionTrait,
{
    let mut active: attendance::ActiveModel = row.into();
    active.work_date = Set(segment.work_date);
    active.check_in_time = Set(Some(segment.check_in_time));
    active.check_out_time = Set(Some(segment.check_out_time));
    active.check_in_at = Set(Some(instants.check_in_at));
    active.check_out_at = Set(Some(instants.check_out_at));
    active.check_in_lat = Set(None);
    active.check_in_lng = Set(None);
    active.check_out_lat = Set(None);
    active.check_out_lng = Set(None);
    active.source = Set(Some(MANUAL_ATTENDANCE_SOURCE.into()));
    active.status = Set(Some(ATTENDANCE_STATUS_COMPLETE.into()));
    active.regularization_status = Set(Some(regularization_status.into()));
    active.updated_at = Set(now);
    active.update(db).await.map_err(KabiPayError::from)
}

#[allow(async_fn_in_trait)]
trait AttendanceRegularizationStore {
    async fn lock_employee_dates(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        dates: &[NaiveDate],
    ) -> KabiPayResult<()>;
    async fn attendance_by_id(
        &mut self,
        tenant_id: Uuid,
        attendance_id: Uuid,
    ) -> KabiPayResult<Option<attendance::Model>>;
    async fn validate_segment(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        segment: SegmentTimes,
        excluded_attendance_id: Option<Uuid>,
        bypass_self_service_age_window: bool,
        clock: TenantBusinessClock,
        actual_dates: Option<(NaiveDate, NaiveDate)>,
        now: DateTime<Utc>,
    ) -> KabiPayResult<SegmentInstants>;
    async fn insert_segment(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        segment: SegmentTimes,
        instants: SegmentInstants,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model>;
    async fn update_segment(
        &mut self,
        row: attendance::Model,
        segment: SegmentTimes,
        instants: SegmentInstants,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model>;
    async fn insert_audit(&mut self, audit: AttendanceAuditInsert) -> KabiPayResult<()>;
}

impl AttendanceRegularizationStore for DatabaseTransaction {
    async fn lock_employee_dates(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        dates: &[NaiveDate],
    ) -> KabiPayResult<()> {
        lock_employee_dates(self, tenant_id, employee_id, dates).await
    }

    async fn attendance_by_id(
        &mut self,
        tenant_id: Uuid,
        attendance_id: Uuid,
    ) -> KabiPayResult<Option<attendance::Model>> {
        attendance::Entity::find_by_id(attendance_id)
            .filter(attendance::Column::TenantId.eq(tenant_id))
            .one(self)
            .await
            .map_err(KabiPayError::from)
    }

    async fn validate_segment(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        segment: SegmentTimes,
        excluded_attendance_id: Option<Uuid>,
        bypass_self_service_age_window: bool,
        clock: TenantBusinessClock,
        actual_dates: Option<(NaiveDate, NaiveDate)>,
        now: DateTime<Utc>,
    ) -> KabiPayResult<SegmentInstants> {
        validate_segment_with_connection(
            self,
            tenant_id,
            employee_id,
            segment,
            excluded_attendance_id,
            bypass_self_service_age_window,
            clock, actual_dates, now,
        )
        .await
    }

    async fn insert_segment(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        segment: SegmentTimes,
        instants: SegmentInstants,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model> {
        insert_manual_segment(
            self,
            tenant_id,
            employee_id,
            segment,
            instants,
            regularization_status,
            now,
        )
        .await
    }

    async fn update_segment(
        &mut self,
        row: attendance::Model,
        segment: SegmentTimes,
        instants: SegmentInstants,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model> {
        update_manual_segment(self, row, segment, instants, regularization_status, now).await
    }

    async fn insert_audit(&mut self, audit: AttendanceAuditInsert) -> KabiPayResult<()> {
        attendance_adjustment_audit::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(audit.tenant_id),
            attendance_id: Set(audit.attendance_id),
            target_employee_id: Set(audit.target_employee_id),
            actor_user_id: Set(audit.actor_user_id),
            operation: Set(audit.operation.as_str().into()),
            reason: Set(audit.reason),
            before_values: Set(audit.before_values),
            after_values: Set(audit.after_values),
            request_id: Set(audit.request_id),
            created_at: Set(audit.created_at),
        }
        .insert(self)
        .await?;
        Ok(())
    }
}

async fn orchestrate_managed_create<S>(
    store: &mut S,
    command: &ManagedCreateCommand,
    now: DateTime<Utc>,
) -> KabiPayResult<attendance::Model>
where
    S: AttendanceRegularizationStore,
{
    let reason = validate_reason(&command.reason)?;
    store
        .lock_employee_dates(
            command.tenant_id,
            command.target_employee_id,
            &[command.segment.work_date],
        )
        .await?;
    let instants = store
        .validate_segment(
            command.tenant_id,
            command.target_employee_id,
            command.segment,
            None,
            true,
            command.clock, command.actual_dates, now,
        )
        .await?;
    let created = store
        .insert_segment(
            command.tenant_id,
            command.target_employee_id,
            command.segment,
            instants,
            MANUAL_REGULARIZED,
            now,
        )
        .await?;
    let after_values = serde_json::to_value(AttendanceAuditSnapshot::try_from(&created)?)?;
    store
        .insert_audit(AttendanceAuditInsert {
            tenant_id: command.tenant_id,
            attendance_id: created.id,
            target_employee_id: command.target_employee_id,
            actor_user_id: command.actor_user_id,
            operation: AttendanceAuditOperation::Create,
            reason,
            before_values: None,
            after_values,
            request_id: command.request_id.clone(),
            created_at: now,
        })
        .await?;
    Ok(created)
}

async fn orchestrate_managed_update<S>(
    store: &mut S,
    command: &ManagedUpdateCommand,
    now: DateTime<Utc>,
) -> KabiPayResult<attendance::Model>
where
    S: AttendanceRegularizationStore,
{
    let reason = validate_reason(&command.reason)?;
    let dates = lock_dates(command.initial_work_date, command.segment.work_date);
    store
        .lock_employee_dates(
            command.tenant_id,
            command.target_employee_id,
            &dates,
        )
        .await?;
    let before = store
        .attendance_by_id(command.tenant_id, command.attendance_id)
        .await?
        .ok_or_else(|| KabiPayError::Forbidden(ATTENDANCE_MANAGEMENT_ACCESS_DENIED.into()))?;
    if before.employee_id != command.target_employee_id {
        return Err(KabiPayError::Forbidden(
            ATTENDANCE_MANAGEMENT_ACCESS_DENIED.into(),
        ));
    }
    if before.updated_at != command.expected_updated_at {
        return Err(KabiPayError::Conflict(
            "attendance segment changed; refresh before retrying".into(),
        ));
    }
    assert_locked_attendance_identity(command.target_employee_id, command.initial_work_date, before.employee_id, before.work_date)?;
    let before_values = serde_json::to_value(AttendanceAuditSnapshot::try_from(&before)?)?;
    let instants = store
        .validate_segment(
            command.tenant_id,
            command.target_employee_id,
            command.segment,
            Some(command.attendance_id),
            true,
            command.clock, command.actual_dates, now,
        )
        .await?;
    let updated = store
        .update_segment(
            before,
            command.segment,
            instants,
            MANUAL_REGULARIZED,
            now,
        )
        .await?;
    let after_values = serde_json::to_value(AttendanceAuditSnapshot::try_from(&updated)?)?;
    store
        .insert_audit(AttendanceAuditInsert {
            tenant_id: command.tenant_id,
            attendance_id: updated.id,
            target_employee_id: command.target_employee_id,
            actor_user_id: command.actor_user_id,
            operation: AttendanceAuditOperation::Update,
            reason,
            before_values: Some(before_values),
            after_values,
            request_id: command.request_id.clone(),
            created_at: now,
        })
        .await?;
    Ok(updated)
}

/// Writes one managed segment and its immutable audit in the caller-owned transaction.
pub async fn create_managed_attendance_segment_in_transaction(
    txn: &mut DatabaseTransaction,
    command: &ManagedCreateCommand,
) -> KabiPayResult<attendance::Model> {
    lock_employee_dates(txn, command.tenant_id, command.target_employee_id, &[command.segment.work_date]).await?;
    orchestrate_managed_create(txn, command, Utc::now()).await
}

pub(crate) async fn update_managed_attendance_segment_in_transaction(
    txn: &mut DatabaseTransaction,
    command: &ManagedUpdateCommand,
) -> KabiPayResult<attendance::Model> {
    lock_employee_dates(txn, command.tenant_id, command.target_employee_id, &[command.initial_work_date, command.segment.work_date]).await?;
    orchestrate_managed_update(txn, command, Utc::now()).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
    use kabipay_db_entities::tenant::d0010_time_shift_roster::attendance;
    use serde_json::json;
    use uuid::Uuid;

    const TENANT_ID: Uuid = Uuid::from_u128(1);
    const EMPLOYEE_ID: Uuid = Uuid::from_u128(2);
    const ACTOR_USER_ID: Uuid = Uuid::from_u128(3);
    const ATTENDANCE_ID: Uuid = Uuid::from_u128(4);

    #[test]
    fn attendance_day_actual_dates_allow_after_midnight_correction_in_original_window() {
        let window = crate::services::attendance_day::resolve_window(&[
            crate::services::attendance_day::PolicyVersion {
                id: Uuid::from_u128(9), effective_work_date: date(2026, 1, 1),
                boundary_minutes: 300, timezone: "Asia/Kolkata".into(),
            }
        ], date(2026, 9, 11)).unwrap();
        let segment = SegmentTimes::for_manual_input(date(2026, 9, 11), time(2, 0), time(4, 0));
        let actual = segment.to_window_instants(&window, Some((date(2026, 9, 12), date(2026, 9, 12)))).unwrap();
        assert_eq!(actual.check_in_at, "2026-09-11T20:30:00Z".parse::<DateTime<Utc>>().unwrap());
        assert_eq!(actual.check_out_at, "2026-09-11T22:30:00Z".parse::<DateTime<Utc>>().unwrap());
        let past_end = SegmentTimes::for_manual_input(date(2026, 9, 11), time(2, 0), time(5, 1));
        assert!(past_end.to_window_instants(&window, Some((date(2026, 9, 12), date(2026, 9, 12)))).is_err());
        let at_end = SegmentTimes::for_manual_input(date(2026, 9, 11), time(2, 0), time(5, 0));
        assert!(at_end.to_window_instants(&window, Some((date(2026, 9, 12), date(2026, 9, 12)))).is_ok());
    }

    #[test]
    fn attendance_day_managed_snapshot_accepts_original_incomplete_without_inventing_checkout() {
        let mut row = attendance_model(date(2026, 8, 24), timestamp(9));
        row.check_out_time = None;
        row.status = Some("INCOMPLETE".into());
        let snapshot = AttendanceAuditSnapshot::try_from(&row).expect("incomplete must be correctable through original ID");
        let json = serde_json::to_value(snapshot).unwrap();
        assert!(json["check_out_time"].is_null());
    }

    #[test]
    fn attendance_day_actual_intervals_reject_overnight_overlap_and_excess_hours_but_exclude_original_id() {
        let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
        let segment = SegmentTimes::for_manual_input(date(2026, 9, 11), time(2, 0), time(4, 0));
        let instants = SegmentInstants { check_in_at: "2026-09-11T20:30:00Z".parse().unwrap(), check_out_at: "2026-09-11T22:30:00Z".parse().unwrap() };
        let mut existing = attendance_model(date(2026, 9, 11), timestamp(9));
        existing.check_in_at = Some("2026-09-11T19:30:00Z".parse().unwrap());
        existing.check_out_at = Some("2026-09-11T21:30:00Z".parse().unwrap());
        assert!(validate_resolved_segment_against_rows(segment, instants, clock, date(2026, 9, 12), 5, &[existing.clone()], None, false).is_err());
        assert!(validate_resolved_segment_against_rows(segment, instants, clock, date(2026, 9, 12), 5, &[existing], Some(ATTENDANCE_ID), false).is_ok());
        assert!(matches!(validate_resolved_segment_against_rows(segment, instants, clock, date(2026, 9, 20), 5, &[], None, false), Err(KabiPayError::Forbidden(_))));
        let too_long = SegmentInstants { check_in_at: "2026-09-10T23:30:00Z".parse().unwrap(), check_out_at: "2026-09-11T23:30:00Z".parse().unwrap() };
        assert!(validate_resolved_segment_against_rows(segment, too_long, clock, date(2026, 9, 12), 5, &[], None, true).is_err());
    }

    #[test]
    fn attendance_day_legacy_inputs_remain_unambiguous_and_long_transition_requires_actual_dates() {
        let policy = |date, boundary| crate::services::attendance_day::PolicyVersion {
            id: Uuid::new_v4(), effective_work_date: date, boundary_minutes: boundary, timezone: "Asia/Kolkata".into(),
        };
        let legacy = crate::services::attendance_day::resolve_window(&[policy(date(2026, 1, 1), 0)], date(2026, 9, 11)).unwrap();
        let segment = SegmentTimes::for_manual_input(date(2026, 9, 11), time(2, 0), time(4, 0));
        assert_eq!(segment.to_window_instants(&legacy, None).unwrap().check_in_at, "2026-09-10T20:30:00Z".parse::<DateTime<Utc>>().unwrap());
        assert!(SegmentTimes::for_manual_input(date(2026, 9, 11), time(23, 0), time(4, 0)).to_window_instants(&legacy, None).is_err());
        let transition = crate::services::attendance_day::resolve_window(&[policy(date(2026, 1, 1), 300), policy(date(2026, 9, 11), 360)], date(2026, 9, 11)).unwrap();
        let ambiguous = SegmentTimes::for_manual_input(date(2026, 9, 11), time(5, 10), time(5, 20));
        assert!(ambiguous.to_window_instants(&transition, None).is_err());
        assert!(ambiguous.to_window_instants(&transition, Some((date(2026, 9, 12), date(2026, 9, 12)))).is_ok());
    }

    fn date(year: i32, month: u32, day: u32) -> NaiveDate {
        NaiveDate::from_ymd_opt(year, month, day).expect("test date must be valid")
    }

    fn time(hour: u32, minute: u32) -> NaiveTime {
        NaiveTime::from_hms_opt(hour, minute, 0).expect("test time must be valid")
    }

    fn timestamp(hour: u32) -> chrono::DateTime<Utc> {
        Utc.with_ymd_and_hms(2026, 8, 24, hour, 0, 0)
            .single()
            .expect("test timestamp must be valid")
    }

    fn attendance_model(
        work_date: NaiveDate,
        updated_at: chrono::DateTime<Utc>,
    ) -> attendance::Model {
        attendance::Model {
            id: ATTENDANCE_ID,
            tenant_id: TENANT_ID,
            employee_id: EMPLOYEE_ID,
            shift_id: None,
            work_date,
            check_in_time: Some(time(9, 0)),
            check_out_time: Some(time(17, 0)),
            check_in_at: None,
            check_out_at: None,
            check_in_lat: None,
            check_in_lng: None,
            check_out_lat: None,
            check_out_lng: None,
            source: Some("WEB+MANUAL".into()),
            status: Some("COMPLETE".into()),
            regularization_status: Some("SELF_REPORTED".into()),
            biometric_ref: None,
            overtime_hours: None,
            late_minutes: None,
            early_exit_minutes: None,
            created_at: timestamp(8),
            updated_at,
        }
    }

    #[test]
    fn managed_reason_is_trimmed_and_must_have_five_to_five_hundred_characters() {
        assert!(validate_reason("abcd").is_err());
        assert_eq!(
            validate_reason("  payroll correction  ").expect("valid reason"),
            "payroll correction"
        );
        assert!(validate_reason(&"x".repeat(500)).is_ok());
        assert!(validate_reason(&"x".repeat(501)).is_err());
    }

    #[test]
    fn audit_snapshot_serializes_only_the_fixed_contract() {
        let row = attendance_model(date(2026, 8, 20), timestamp(12));

        let snapshot = AttendanceAuditSnapshot::try_from(&row).expect("valid snapshot");
        let serialized = serde_json::to_value(snapshot).expect("snapshot must serialize");

        assert_eq!(
            serialized,
            json!({
                "work_date": "2026-08-20",
                "check_in_time": "09:00:00",
                "check_out_time": "17:00:00",
                "check_in_at": null,
                "check_out_at": null,
                "status": "COMPLETE",
                "source": "WEB+MANUAL",
                "regularization_status": "SELF_REPORTED",
                "updated_at": "2026-08-24T12:00:00Z"
            })
        );
    }

    #[test]
    fn moved_segment_dates_are_sorted_and_deduplicated_before_locking() {
        let old_date = date(2026, 8, 24);
        let new_date = date(2026, 8, 20);

        assert_eq!(lock_dates(old_date, new_date), vec![new_date, old_date]);
        assert_eq!(lock_dates(old_date, old_date), vec![old_date]);
    }

    #[test]
    fn self_update_rejects_post_lock_employee_or_source_date_drift() {
        let locked_date = date(2026, 8, 20);

        for (current_employee_id, current_work_date) in [
            (Uuid::from_u128(99), locked_date),
            (EMPLOYEE_ID, date(2026, 8, 21)),
        ] {
            assert!(matches!(
                assert_locked_attendance_identity(
                    EMPLOYEE_ID,
                    locked_date,
                    current_employee_id,
                    current_work_date,
                ),
                Err(KabiPayError::Conflict(_))
            ));
        }

        assert!(assert_locked_attendance_identity(
            EMPLOYEE_ID,
            locked_date,
            EMPLOYEE_ID,
            locked_date,
        )
        .is_ok());
    }

    #[test]
    fn segment_validation_rejects_future_and_invalid_time_order() {
        let today = date(2026, 8, 24);
        let future = SegmentTimes {
            work_date: date(2026, 8, 25),
            check_in_time: time(9, 0),
            check_out_time: time(17, 0),
        };
        let invalid_order = SegmentTimes {
            work_date: today,
            check_in_time: time(17, 0),
            check_out_time: time(9, 0),
        };

        assert!(matches!(
            validate_segment_against_rows(future, today, 5, &[], None, false),
            Err(KabiPayError::Validation(_))
        ));
        assert!(matches!(
            validate_segment_against_rows(invalid_order, today, 5, &[], None, false),
            Err(KabiPayError::Validation(_))
        ));
    }

    #[test]
    fn manual_segment_times_are_normalized_to_minute_precision() {
        let segment = SegmentTimes::for_manual_input(
            date(2026, 8, 24),
            NaiveTime::from_hms_opt(9, 0, 45).expect("valid check-in time"),
            NaiveTime::from_hms_nano_opt(17, 30, 59, 999_999_999)
                .expect("valid check-out time"),
        );

        assert_eq!(segment.check_in_time, time(9, 0));
        assert_eq!(segment.check_out_time, time(17, 30));
    }

    #[test]
    fn legacy_manual_rows_use_minute_precision_but_live_rows_keep_seconds() {
        let work_date = date(2026, 8, 24);
        let requested = SegmentTimes::for_manual_input(work_date, time(9, 0), time(10, 0));
        let mut existing = attendance_model(work_date, timestamp(12));
        existing.check_in_time = Some(
            NaiveTime::from_hms_opt(8, 0, 45).expect("valid legacy manual check-in"),
        );
        existing.check_out_time = Some(
            NaiveTime::from_hms_opt(9, 0, 45).expect("valid legacy manual check-out"),
        );

        assert!(validate_segment_against_rows(
            requested,
            work_date,
            5,
            &[existing.clone()],
            None,
            true,
        )
        .is_ok());

        existing.source = Some("BIOMETRIC".into());
        assert!(matches!(
            validate_segment_against_rows(
                requested,
                work_date,
                5,
                &[existing],
                None,
                true,
            ),
            Err(KabiPayError::Validation(_))
        ));
    }

    #[test]
    fn segment_validation_rejects_overlap_and_open_punch() {
        let work_date = date(2026, 8, 24);
        let segment = SegmentTimes {
            work_date,
            check_in_time: time(10, 0),
            check_out_time: time(11, 0),
        };
        let overlap = attendance_model(work_date, timestamp(12));
        let mut open_punch = attendance_model(work_date, timestamp(12));
        open_punch.check_in_time = Some(time(8, 0));
        open_punch.check_out_time = None;

        assert!(matches!(
            validate_segment_against_rows(segment, work_date, 5, &[overlap], None, false),
            Err(KabiPayError::Validation(_))
        ));
        assert!(matches!(
            validate_segment_against_rows(segment, work_date, 5, &[open_punch], None, false),
            Err(KabiPayError::Validation(_))
        ));
    }

    #[test]
    fn segment_validation_rejects_daily_cap() {
        let work_date = date(2026, 8, 24);
        let mut first = attendance_model(work_date, timestamp(12));
        first.check_in_time = Some(time(0, 0));
        first.check_out_time = Some(time(12, 0));
        let mut second = first.clone();
        second.id = Uuid::from_u128(5);
        let segment = SegmentTimes {
            work_date,
            check_in_time: time(12, 0),
            check_out_time: time(13, 0),
        };

        assert!(matches!(
            validate_segment_against_rows(
                segment,
                work_date,
                5,
                &[first, second],
                None,
                false,
            ),
            Err(KabiPayError::Validation(_))
        ));
    }

    #[test]
    fn managed_validation_bypasses_only_the_self_service_age_window() {
        let today = date(2026, 8, 24);
        let old_segment = SegmentTimes {
            work_date: date(2026, 8, 10),
            check_in_time: time(9, 0),
            check_out_time: time(17, 0),
        };

        assert!(matches!(
            validate_segment_against_rows(old_segment, today, 5, &[], None, false),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(validate_segment_against_rows(old_segment, today, 5, &[], None, true).is_ok());
    }

    #[derive(Clone, Debug, PartialEq)]
    enum Operation {
        Lock(Vec<NaiveDate>),
        Load,
        Validate { bypass_self_service_age_window: bool },
        Insert,
        Update,
        Audit(AttendanceAuditInsert),
    }

    struct FakeStore {
        row: Option<attendance::Model>,
        operations: Vec<Operation>,
    }

    impl FakeStore {
        fn new(row: Option<attendance::Model>) -> Self {
            Self {
                row,
                operations: Vec::new(),
            }
        }
    }

    impl AttendanceRegularizationStore for FakeStore {
        async fn lock_employee_dates(
            &mut self,
            _tenant_id: Uuid,
            _employee_id: Uuid,
            dates: &[NaiveDate],
        ) -> kabipay_common::KabiPayResult<()> {
            self.operations.push(Operation::Lock(dates.to_vec()));
            Ok(())
        }

        async fn attendance_by_id(
            &mut self,
            _tenant_id: Uuid,
            _attendance_id: Uuid,
        ) -> kabipay_common::KabiPayResult<Option<attendance::Model>> {
            self.operations.push(Operation::Load);
            Ok(self.row.clone())
        }

        async fn validate_segment(
            &mut self,
            _tenant_id: Uuid,
            _employee_id: Uuid,
            segment: SegmentTimes,
            _excluded_attendance_id: Option<Uuid>,
            bypass_self_service_age_window: bool,
            clock: TenantBusinessClock,
            _actual_dates: Option<(NaiveDate, NaiveDate)>,
            _now: DateTime<Utc>,
        ) -> kabipay_common::KabiPayResult<SegmentInstants> {
            self.operations.push(Operation::Validate {
                bypass_self_service_age_window,
            });
            segment.to_instants(clock)
        }

        async fn insert_segment(
            &mut self,
            tenant_id: Uuid,
            employee_id: Uuid,
            segment: SegmentTimes,
            instants: SegmentInstants,
            regularization_status: &'static str,
            now: chrono::DateTime<Utc>,
        ) -> kabipay_common::KabiPayResult<attendance::Model> {
            self.operations.push(Operation::Insert);
            let mut row = attendance_model(segment.work_date, now);
            row.tenant_id = tenant_id;
            row.employee_id = employee_id;
            row.check_in_time = Some(segment.check_in_time);
            row.check_out_time = Some(segment.check_out_time);
            row.check_in_at = Some(instants.check_in_at);
            row.check_out_at = Some(instants.check_out_at);
            row.regularization_status = Some(regularization_status.into());
            self.row = Some(row.clone());
            Ok(row)
        }

        async fn update_segment(
            &mut self,
            mut row: attendance::Model,
            segment: SegmentTimes,
            instants: SegmentInstants,
            regularization_status: &'static str,
            now: chrono::DateTime<Utc>,
        ) -> kabipay_common::KabiPayResult<attendance::Model> {
            self.operations.push(Operation::Update);
            row.work_date = segment.work_date;
            row.check_in_time = Some(segment.check_in_time);
            row.check_out_time = Some(segment.check_out_time);
            row.check_in_at = Some(instants.check_in_at);
            row.check_out_at = Some(instants.check_out_at);
            row.regularization_status = Some(regularization_status.into());
            row.updated_at = now;
            self.row = Some(row.clone());
            Ok(row)
        }

        async fn insert_audit(
            &mut self,
            audit: AttendanceAuditInsert,
        ) -> kabipay_common::KabiPayResult<()> {
            self.operations.push(Operation::Audit(audit));
            Ok(())
        }
    }

    fn managed_update_command(expected_updated_at: chrono::DateTime<Utc>) -> ManagedUpdateCommand {
        let segment = SegmentTimes {
            work_date: date(2026, 8, 20),
            check_in_time: time(10, 0),
            check_out_time: time(18, 0),
        };
        ManagedUpdateCommand {
            tenant_id: TENANT_ID,
            attendance_id: ATTENDANCE_ID,
            target_employee_id: EMPLOYEE_ID,
            actor_user_id: ACTOR_USER_ID,
            initial_work_date: date(2026, 8, 24),
            segment,
            clock: TenantBusinessClock::from_name("UTC").unwrap(),
            actual_dates: None,
            reason: "  approved payroll correction  ".into(),
            request_id: Some("request-123".into()),
            expected_updated_at,
        }
    }

    #[tokio::test]
    async fn stale_managed_update_returns_conflict_before_attendance_or_audit_write() {
        let current_updated_at = timestamp(12);
        let mut store = FakeStore::new(Some(attendance_model(
            date(2026, 8, 24),
            current_updated_at,
        )));

        let result = orchestrate_managed_update(
            &mut store,
            &managed_update_command(timestamp(11)),
            timestamp(13),
        )
        .await;

        assert!(matches!(result, Err(kabipay_common::KabiPayError::Conflict(_))));
        assert_eq!(
            store.operations,
            vec![
                Operation::Lock(vec![date(2026, 8, 20), date(2026, 8, 24)]),
                Operation::Load,
            ]
        );
    }

    #[tokio::test]
    async fn managed_create_orchestrates_lock_validation_write_and_create_audit() {
        let mut store = FakeStore::new(None);
        let segment = SegmentTimes {
            work_date: date(2026, 8, 20),
            check_in_time: time(9, 30),
            check_out_time: time(17, 30),
        };
        let command = ManagedCreateCommand {
            tenant_id: TENANT_ID,
            target_employee_id: EMPLOYEE_ID,
            actor_user_id: ACTOR_USER_ID,
            segment,
            clock: TenantBusinessClock::from_name("UTC").unwrap(),
            actual_dates: None,
            reason: "  approved missed punch  ".into(),
            request_id: Some("request-123".into()),
        };

        let created = orchestrate_managed_create(&mut store, &command, timestamp(13))
            .await
            .expect("managed create must succeed");

        assert_eq!(created.regularization_status.as_deref(), Some("REGULARIZED"));
        assert!(matches!(
            store.operations.as_slice(),
            [
                Operation::Lock(dates),
                Operation::Validate {
                    bypass_self_service_age_window: true
                },
                Operation::Insert,
                Operation::Audit(AttendanceAuditInsert {
                    operation: AttendanceAuditOperation::Create,
                    reason,
                    before_values: None,
                    request_id: Some(request_id),
                    ..
                })
            ] if dates == &vec![date(2026, 8, 20)]
                && reason == "approved missed punch"
                && request_id == "request-123"
        ));
    }

    #[tokio::test]
    async fn managed_update_orchestrates_sorted_locks_validation_write_and_update_audit() {
        let before = attendance_model(date(2026, 8, 24), timestamp(12));
        let mut store = FakeStore::new(Some(before));

        let updated = orchestrate_managed_update(
            &mut store,
            &managed_update_command(timestamp(12)),
            timestamp(13),
        )
        .await
        .expect("managed update must succeed");

        assert_eq!(updated.work_date, date(2026, 8, 20));
        assert_eq!(updated.regularization_status.as_deref(), Some("REGULARIZED"));
        assert!(matches!(
            store.operations.as_slice(),
            [
                Operation::Lock(dates),
                Operation::Load,
                Operation::Validate {
                    bypass_self_service_age_window: true
                },
                Operation::Update,
                Operation::Audit(AttendanceAuditInsert {
                    operation: AttendanceAuditOperation::Update,
                    reason,
                    before_values: Some(_),
                    ..
                })
            ] if dates == &vec![date(2026, 8, 20), date(2026, 8, 24)]
                && reason == "approved payroll correction"
        ));
    }
}
