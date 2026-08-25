//! Transaction-scoped attendance adjustment validation and audit writes.

use chrono::{DateTime, NaiveDate, NaiveTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
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

#[derive(Clone, Debug)]
pub struct ManagedCreateCommand {
    pub tenant_id: Uuid,
    pub target_employee_id: Uuid,
    pub actor_user_id: Uuid,
    pub segment: SegmentTimes,
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
    pub check_out_time: NaiveTime,
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
            check_out_time: row.check_out_time.ok_or_else(|| {
                KabiPayError::Validation("attendance segment has no check-out time".into())
            })?,
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

fn segment_minutes(check_in_time: NaiveTime, check_out_time: NaiveTime) -> KabiPayResult<i32> {
    use chrono::Timelike;

    if check_in_time >= check_out_time {
        return Err(KabiPayError::Validation(
            "checkInTime must be before checkOutTime (same-day segment only)".into(),
        ));
    }
    let seconds = i64::from(check_out_time.num_seconds_from_midnight())
        - i64::from(check_in_time.num_seconds_from_midnight());
    i32::try_from(seconds / 60)
        .map_err(|_| KabiPayError::Internal("attendance segment duration overflow".into()))
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

fn validate_segment_date_and_time(segment: SegmentTimes, today: NaiveDate) -> KabiPayResult<i32> {
    if segment.work_date > today {
        return Err(KabiPayError::Validation(
            "workDate cannot be in the future".into(),
        ));
    }
    segment_minutes(segment.check_in_time, segment.check_out_time)
}

fn validate_segment_against_rows(
    segment: SegmentTimes,
    today: NaiveDate,
    max_self_adjust_days: i64,
    existing: &[attendance::Model],
    excluded_attendance_id: Option<Uuid>,
    bypass_self_service_age_window: bool,
) -> KabiPayResult<()> {
    let requested_minutes = validate_segment_date_and_time(segment, today)?;
    let days_since = today.signed_duration_since(segment.work_date).num_days();
    let window = max_self_adjust_days.max(0);
    if days_since > window && !bypass_self_service_age_window {
        return Err(KabiPayError::Forbidden(format!(
            "manual attendance is limited to the last {} calendar days unless you hold attendance regularization permission",
            window
        )));
    }

    let mut total_minutes = requested_minutes;
    for row in existing {
        if excluded_attendance_id == Some(row.id) {
            continue;
        }
        match (row.check_in_time, row.check_out_time) {
            (Some(existing_in), Some(existing_out)) => {
                if segment.check_in_time < existing_out && segment.check_out_time > existing_in {
                    return Err(KabiPayError::Validation(
                        "manual attendance overlaps with an existing segment for this day".into(),
                    ));
                }
                total_minutes = total_minutes
                    .checked_add(segment_minutes(existing_in, existing_out)?)
                    .ok_or_else(|| {
                        KabiPayError::Internal("attendance daily duration overflow".into())
                    })?;
            }
            (Some(_), None) => {
                return Err(KabiPayError::Validation(
                    "complete the open punch before adjusting manual attendance for this day"
                        .into(),
                ));
            }
            _ => {}
        }
    }
    assert_total_attendance_minutes_under_daily_cap(total_minutes)
}

pub(crate) async fn validate_segment_with_connection<C>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    segment: SegmentTimes,
    excluded_attendance_id: Option<Uuid>,
    bypass_self_service_age_window: bool,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    let today = Utc::now().date_naive();
    validate_segment_date_and_time(segment, today)?;
    let policy = hrms_master_service::load_attendance_adjustment_policy(db, tenant_id).await?;
    let existing = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(attendance::Column::EmployeeId.eq(employee_id))
        .filter(attendance::Column::WorkDate.eq(segment.work_date))
        .order_by_asc(attendance::Column::CreatedAt)
        .all(db)
        .await?;
    validate_segment_against_rows(
        segment,
        today,
        policy.max_self_adjust_days,
        &existing,
        excluded_attendance_id,
        bypass_self_service_age_window,
    )
}

/// Acquires transaction-scoped locks for one employee's dates in stable order.
pub async fn lock_employee_dates(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    employee_id: Uuid,
    dates: &[NaiveDate],
) -> KabiPayResult<()> {
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
    ) -> KabiPayResult<()>;
    async fn insert_segment(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        segment: SegmentTimes,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model>;
    async fn update_segment(
        &mut self,
        row: attendance::Model,
        segment: SegmentTimes,
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
    ) -> KabiPayResult<()> {
        validate_segment_with_connection(
            self,
            tenant_id,
            employee_id,
            segment,
            excluded_attendance_id,
            bypass_self_service_age_window,
        )
        .await
    }

    async fn insert_segment(
        &mut self,
        tenant_id: Uuid,
        employee_id: Uuid,
        segment: SegmentTimes,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model> {
        insert_manual_segment(
            self,
            tenant_id,
            employee_id,
            segment,
            regularization_status,
            now,
        )
        .await
    }

    async fn update_segment(
        &mut self,
        row: attendance::Model,
        segment: SegmentTimes,
        regularization_status: &'static str,
        now: DateTime<Utc>,
    ) -> KabiPayResult<attendance::Model> {
        update_manual_segment(self, row, segment, regularization_status, now).await
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
    store
        .validate_segment(
            command.tenant_id,
            command.target_employee_id,
            command.segment,
            None,
            true,
        )
        .await?;
    let created = store
        .insert_segment(
            command.tenant_id,
            command.target_employee_id,
            command.segment,
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
    let before_values = serde_json::to_value(AttendanceAuditSnapshot::try_from(&before)?)?;
    store
        .validate_segment(
            command.tenant_id,
            command.target_employee_id,
            command.segment,
            Some(command.attendance_id),
            true,
        )
        .await?;
    let updated = store
        .update_segment(before, command.segment, MANUAL_REGULARIZED, now)
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
    orchestrate_managed_create(txn, command, Utc::now()).await
}

pub(crate) async fn update_managed_attendance_segment_in_transaction(
    txn: &mut DatabaseTransaction,
    command: &ManagedUpdateCommand,
) -> KabiPayResult<attendance::Model> {
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
            _segment: SegmentTimes,
            _excluded_attendance_id: Option<Uuid>,
            bypass_self_service_age_window: bool,
        ) -> kabipay_common::KabiPayResult<()> {
            self.operations.push(Operation::Validate {
                bypass_self_service_age_window,
            });
            Ok(())
        }

        async fn insert_segment(
            &mut self,
            tenant_id: Uuid,
            employee_id: Uuid,
            segment: SegmentTimes,
            regularization_status: &'static str,
            now: chrono::DateTime<Utc>,
        ) -> kabipay_common::KabiPayResult<attendance::Model> {
            self.operations.push(Operation::Insert);
            let mut row = attendance_model(segment.work_date, now);
            row.tenant_id = tenant_id;
            row.employee_id = employee_id;
            row.check_in_time = Some(segment.check_in_time);
            row.check_out_time = Some(segment.check_out_time);
            row.regularization_status = Some(regularization_status.into());
            self.row = Some(row.clone());
            Ok(row)
        }

        async fn update_segment(
            &mut self,
            mut row: attendance::Model,
            segment: SegmentTimes,
            regularization_status: &'static str,
            now: chrono::DateTime<Utc>,
        ) -> kabipay_common::KabiPayResult<attendance::Model> {
            self.operations.push(Operation::Update);
            row.work_date = segment.work_date;
            row.check_in_time = Some(segment.check_in_time);
            row.check_out_time = Some(segment.check_out_time);
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
        ManagedUpdateCommand {
            tenant_id: TENANT_ID,
            attendance_id: ATTENDANCE_ID,
            target_employee_id: EMPLOYEE_ID,
            actor_user_id: ACTOR_USER_ID,
            initial_work_date: date(2026, 8, 24),
            segment: SegmentTimes {
                work_date: date(2026, 8, 20),
                check_in_time: time(10, 0),
                check_out_time: time(18, 0),
            },
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
        let command = ManagedCreateCommand {
            tenant_id: TENANT_ID,
            target_employee_id: EMPLOYEE_ID,
            actor_user_id: ACTOR_USER_ID,
            segment: SegmentTimes {
                work_date: date(2026, 8, 20),
                check_in_time: time(9, 30),
                check_out_time: time(17, 30),
            },
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
