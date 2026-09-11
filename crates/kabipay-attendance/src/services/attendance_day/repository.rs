//! Transaction-owned policy activation, scheduling and window freezing.
use super::{calendar::{invalid, validate_minutes}, *};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{context::{ClientClaims, ScopeType, PERM_ATTENDANCE_PUNCH_POLICY}, tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, QueryResult, Statement, Value};
use uuid::Uuid;

#[derive(Clone, Debug)]
pub struct AttendanceDayPolicy {
    pub revision: i64,
    pub initialized: bool,
    pub legacy_activation_pending: bool,
    pub legacy_activation_date: Option<NaiveDate>,
    pub versions: Vec<PolicyVersion>,
}
#[derive(Clone, Debug)]
pub struct SchedulePolicyCommand {
    pub expected_revision: i64,
    pub effective_work_date: NaiveDate,
    pub boundary_minutes: i32,
}
fn sql(query: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, query, values)
}

fn frozen_row(row: QueryResult) -> KabiPayResult<AttendanceDayWindow> {
    Ok(AttendanceDayWindow {
        work_date: row.try_get("", "work_date")?,
        starts_at: row.try_get("", "starts_at")?,
        ends_at: row.try_get("", "ends_at")?,
        timezone: row.try_get("", "timezone")?,
        boundary_minutes: row.try_get("", "boundary_minutes")?,
        policy_version_id: row.try_get("", "policy_version_id")?,
    })
}

async fn frozen_for_date<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, work_date: NaiveDate,
) -> KabiPayResult<Option<AttendanceDayWindow>> {
    db.query_one(sql(
        "SELECT work_date, starts_at, ends_at, timezone, boundary_minutes, policy_version_id FROM attendance_day_window WHERE tenant_id = $1 AND work_date = $2",
        vec![tenant_id.into(), work_date.into()],
    )).await?.map(frozen_row).transpose()
}

/// Read-only snapshot; a missing profile never creates a moving activation anchor.
pub async fn policy<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, clock: TenantBusinessClock, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayPolicy> {
    // Profile, live versions AND legacy attendance existence must share one MVCC
    // snapshot: bootstrap can commit a fresh 05:00 profile and attendance together.
    // The scalar existence row survives missing profiles. A profile without live
    // versions still has a revision, so its null version fields fail closed below.
    let rows = db.query_all(sql(
        "SELECT p.revision, p.legacy_activation_date, v.id, v.effective_work_date, v.boundary_minutes, v.timezone, history.has_attendance FROM (SELECT EXISTS (SELECT 1 FROM attendance WHERE tenant_id = $1) AS has_attendance) history LEFT JOIN attendance_day_profile p ON p.tenant_id = $1 LEFT JOIN attendance_day_policy_version v ON v.tenant_id = p.tenant_id AND v.superseded_at IS NULL ORDER BY v.effective_work_date",
        vec![tenant_id.into()],
    )).await?;
    let first = rows.first()
        .ok_or_else(|| invalid("attendance policy snapshot returned no result"))?;
    if let Some(revision) = first.try_get::<Option<i64>>("", "revision")? {
        let legacy_activation_date: Option<NaiveDate> = first.try_get("", "legacy_activation_date")?;
        let versions = rows.iter().map(|row| Ok(PolicyVersion {
            id: row.try_get("", "id")?,
            effective_work_date: row.try_get("", "effective_work_date")?,
            boundary_minutes: row.try_get("", "boundary_minutes")?,
            timezone: row.try_get("", "timezone")?,
        })).collect::<KabiPayResult<Vec<_>>>()?;
        let active = resolve_current_window(&versions, now)?;
        let legacy_activation_pending = legacy_activation_date.is_some()
            && versions.first().is_some_and(|v| v.id == active.policy_version_id);
        return Ok(AttendanceDayPolicy {
            revision, initialized: true, legacy_activation_pending,
            legacy_activation_date, versions,
        });
    }
    // Deleted historical rows still establish legacy interpretation, from the
    // same snapshot that established the profile's absence.
    let has_attendance: bool = first.try_get("", "has_attendance")?;
    Ok(AttendanceDayPolicy {
        revision: 0, initialized: false,
        legacy_activation_pending: has_attendance, legacy_activation_date: None,
        versions: vec![PolicyVersion {
            id: Uuid::nil(),
            effective_work_date: NaiveDate::from_ymd_opt(1, 1, 1)
                .ok_or_else(|| invalid("attendance baseline date is invalid"))?,
            boundary_minutes: if has_attendance { 0 } else { 300 },
            timezone: clock.timezone_name().into(),
        }],
    })
}

pub async fn window_for_date<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, clock: TenantBusinessClock,
    work_date: NaiveDate, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayWindow> {
    if let Some(window) = frozen_for_date(db, tenant_id, work_date).await? {
        return Ok(window);
    }
    resolve_window(&policy(db, tenant_id, clock, now).await?.versions, work_date)
}

pub async fn current_window<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, clock: TenantBusinessClock, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayWindow> {
    if let Some(row) = db.query_one(sql(
        "SELECT work_date, starts_at, ends_at, timezone, boundary_minutes, policy_version_id FROM attendance_day_window WHERE tenant_id = $1 AND starts_at <= $2 AND ends_at > $2 ORDER BY starts_at DESC LIMIT 1",
        vec![tenant_id.into(), now.into()],
    )).await? {
        return frozen_row(row);
    }
    resolve_current_window(&policy(db, tenant_id, clock, now).await?.versions, now)
}

/// Acquire before employee/day locks, then sample the caller's clock after all locks.
pub async fn lock_policy(db: &DatabaseTransaction, tenant_id: Uuid) -> KabiPayResult<()> {
    db.execute(sql(
        "SELECT pg_advisory_xact_lock(hashtextextended($1, 0))",
        vec![format!("attendance-day-policy:{tenant_id}").into()],
    )).await?;
    Ok(())
}

async fn insert_version(
    db: &DatabaseTransaction, tenant_id: Uuid, version: &PolicyVersion, now: DateTime<Utc>,
) -> KabiPayResult<()> {
    db.execute(sql(
        "INSERT INTO attendance_day_policy_version (id, tenant_id, effective_work_date, boundary_minutes, timezone, created_at) VALUES ($1, $2, $3, $4, $5, $6)",
        vec![version.id.into(), tenant_id.into(), version.effective_work_date.into(),
            version.boundary_minutes.into(), version.timezone.clone().into(), now.into()],
    )).await?;
    Ok(())
}

/// Prepare the same bootstrap state for a read-only proposal and a later write.
/// Generated IDs are prospective until the caller persists this state.
fn prepare_initialization(
    clock: TenantBusinessClock,
    mut state: AttendanceDayPolicy, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayPolicy> {
    if state.initialized { return Ok(state); }
    if state.legacy_activation_pending {
        let activation = clock.business_date(now).succ_opt()
            .ok_or_else(|| invalid("attendance activation date overflows"))?;
        state.legacy_activation_date = Some(activation);
        state.versions.push(PolicyVersion {
            id: Uuid::new_v4(), effective_work_date: activation,
            boundary_minutes: 300, timezone: clock.timezone_name().into(),
        });
    }
    state.versions.first_mut()
        .ok_or_else(|| invalid("attendance baseline policy is missing"))?.id = Uuid::new_v4();
    state.revision = 1;
    state.initialized = true;
    Ok(state)
}

async fn persist_initialization(
    db: &DatabaseTransaction, tenant_id: Uuid, state: &AttendanceDayPolicy,
    now: DateTime<Utc>,
) -> KabiPayResult<()> {
    db.execute(sql(
        "INSERT INTO attendance_day_profile (tenant_id, revision, legacy_activation_date, initialized_at) VALUES ($1, 1, $2, $3)",
        vec![tenant_id.into(), state.legacy_activation_date.into(), now.into()],
    )).await?;
    for version in &state.versions { insert_version(db, tenant_id, version, now).await?; }
    Ok(())
}

async fn initialize(
    db: &DatabaseTransaction, tenant_id: Uuid, clock: TenantBusinessClock,
    state: AttendanceDayPolicy, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayPolicy> {
    if state.initialized { return Ok(state); }
    let state = prepare_initialization(clock, state, now)?;
    persist_initialization(db, tenant_id, &state, now).await?;
    Ok(state)
}

pub(super) fn validate_schedule_request(
    tenant_id: Uuid, claims: &ClientClaims, command: &SchedulePolicyCommand,
) -> KabiPayResult<()> {
    if claims.tenant_id != tenant_id
        || !claims.has_any_permission(&[PERM_ATTENDANCE_PUNCH_POLICY])
        || claims.explicit_scope_for_permission(PERM_ATTENDANCE_PUNCH_POLICY) != Some(ScopeType::All)
    {
        return Err(KabiPayError::Forbidden("attendance day policy requires tenant-wide configuration authority".into()));
    }
    validate_minutes(command.boundary_minutes)
}

/// A validated proposal has no durable side effects. Scheduling builds it anew
/// from its locked snapshot; a previously returned preview is never trusted.
pub(super) struct PolicyChangeProposal {
    pub state: AttendanceDayPolicy,
    pub pending: Vec<PolicyVersion>,
    pub version: PolicyVersion,
    pub versions: Vec<PolicyVersion>,
    pub transition: AttendanceDayWindow,
    pub following: AttendanceDayWindow,
    pub next_revision: i64,
}

pub(super) async fn propose_policy_change<C: ConnectionTrait>(
    db: &C, tenant_id: Uuid, clock: TenantBusinessClock,
    state: AttendanceDayPolicy, command: &SchedulePolicyCommand, now: DateTime<Utc>,
) -> KabiPayResult<PolicyChangeProposal> {
    if command.expected_revision != state.revision {
        return Err(KabiPayError::Conflict("attendance day policy changed; refresh before retrying".into()));
    }
    let active = resolve_current_window(&state.versions, now)?;
    if command.effective_work_date <= active.work_date {
        return Err(invalid("attendance day policy must take effect after the active work date"));
    }
    let state = prepare_initialization(clock, state, now)?;
    let pending: Vec<_> = state.versions.iter()
        .filter(|v| v.effective_work_date > active.work_date).cloned().collect();
    let affected_date = pending.iter().map(|v| v.effective_work_date)
        .fold(command.effective_work_date, NaiveDate::min);
    if db.query_one(sql(
        "SELECT work_date FROM attendance_day_window WHERE tenant_id = $1 AND work_date >= $2 LIMIT 1",
        vec![tenant_id.into(), affected_date.into()],
    )).await?.is_some() {
        return Err(KabiPayError::Conflict("attendance day policy would alter a frozen window".into()));
    }
    let version = PolicyVersion {
        id: Uuid::new_v4(), effective_work_date: command.effective_work_date,
        boundary_minutes: command.boundary_minutes, timezone: clock.timezone_name().into(),
    };
    let mut versions: Vec<_> = state.versions.iter()
        .filter(|v| v.effective_work_date <= active.work_date).cloned().collect();
    versions.push(version.clone());
    let transition = resolve_window(&versions, command.effective_work_date)?;
    let next_date = command.effective_work_date.succ_opt()
        .ok_or_else(|| invalid("attendance effective date overflows"))?;
    let following = resolve_window(&versions, next_date)?;
    if transition.starts_at <= now || transition.ends_at != following.starts_at {
        return Err(invalid("attendance policy transition must be future and continuous"));
    }
    let next_revision = state.revision.checked_add(1)
        .ok_or_else(|| invalid("attendance policy revision overflows"))?;
    Ok(PolicyChangeProposal { state, pending, version, versions, transition, following, next_revision })
}

/// Freeze within the caller's transaction; on error the caller must roll back.
pub async fn ensure_window(
    db: &DatabaseTransaction, tenant_id: Uuid, clock: TenantBusinessClock,
    work_date: NaiveDate, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayWindow> {
    lock_policy(db, tenant_id).await?;
    if let Some(window) = frozen_for_date(db, tenant_id, work_date).await? {
        return Ok(window);
    }
    let state = policy(db, tenant_id, clock, now).await?;
    let state = initialize(db, tenant_id, clock, state, now).await?;
    let window = resolve_window(&state.versions, work_date)?;
    let previous = work_date.pred_opt().ok_or_else(|| invalid("attendance date overflows"))?;
    let next = work_date.succ_opt().ok_or_else(|| invalid("attendance date overflows"))?;
    let neighbors = db.query_all(sql(
        "SELECT work_date, starts_at, ends_at FROM attendance_day_window WHERE tenant_id = $1 AND work_date IN ($2, $3)",
        vec![tenant_id.into(), previous.into(), next.into()],
    )).await?;
    for row in neighbors {
        let date: NaiveDate = row.try_get("", "work_date")?;
        let starts_at: DateTime<Utc> = row.try_get("", "starts_at")?;
        let ends_at: DateTime<Utc> = row.try_get("", "ends_at")?;
        if (date == previous && ends_at != window.starts_at)
            || (date == next && starts_at != window.ends_at)
        {
            return Err(KabiPayError::Conflict("attendance window disagrees with frozen neighbor".into()));
        }
    }
    db.execute(sql(
        "INSERT INTO attendance_day_window (id, tenant_id, work_date, starts_at, ends_at, timezone, boundary_minutes, policy_version_id, created_at) VALUES ($1, $2, $3, $4, $5, $6, $7, $8, $9)",
        vec![Uuid::new_v4().into(), tenant_id.into(), work_date.into(), window.starts_at.into(),
            window.ends_at.into(), window.timezone.clone().into(), window.boundary_minutes.into(),
            window.policy_version_id.into(), now.into()],
    )).await?;
    Ok(window)
}

fn audit_state(state: &AttendanceDayPolicy) -> serde_json::Value {
    serde_json::json!({
        "revision": state.revision, "initialized": state.initialized,
        "legacy_activation_date": state.legacy_activation_date,
        "versions": state.versions.iter().map(|v| serde_json::json!({
            "id": v.id, "effective_work_date": v.effective_work_date,
            "boundary_minutes": v.boundary_minutes, "timezone": v.timezone,
        })).collect::<Vec<_>>()
    })
}

/// Schedule one future replacement, preserving superseded rows and auditing the actor.
/// Caller owns commit/rollback and must pass a timestamp sampled after locks.
pub async fn schedule_policy(
    db: &DatabaseTransaction, tenant_id: Uuid, clock: TenantBusinessClock,
    claims: &ClientClaims, command: SchedulePolicyCommand, now: DateTime<Utc>,
) -> KabiPayResult<AttendanceDayPolicy> {
    validate_schedule_request(tenant_id, claims, &command)?;
    lock_policy(db, tenant_id).await?;
    let state = policy(db, tenant_id, clock, now).await?;
    let before = audit_state(&state);
    let needs_initialization = !state.initialized;
    let proposal = propose_policy_change(db, tenant_id, clock, state, &command, now).await?;
    if needs_initialization {
        persist_initialization(db, tenant_id, &proposal.state, now).await?;
    }
    let PolicyChangeProposal {
        mut state, pending, version, versions: proposed, next_revision: revision, ..
    } = proposal;
    let update = db.execute(sql(
        "UPDATE attendance_day_profile SET revision = $2 WHERE tenant_id = $1 AND revision = $3",
        vec![tenant_id.into(), revision.into(), state.revision.into()],
    )).await?;
    if update.rows_affected() != 1 {
        return Err(KabiPayError::Conflict("attendance day policy changed; refresh before retrying".into()));
    }
    for prior in pending {
        let update = db.execute(sql(
            "UPDATE attendance_day_policy_version SET superseded_at = $3 WHERE tenant_id = $1 AND id = $2 AND superseded_at IS NULL",
            vec![tenant_id.into(), prior.id.into(), now.into()],
        )).await?;
        if update.rows_affected() != 1 {
            return Err(KabiPayError::Conflict("attendance pending policy changed".into()));
        }
    }
    insert_version(db, tenant_id, &version, now).await?;
    state.versions = proposed;
    state.revision = revision;
    db.execute(sql(
        "INSERT INTO audit_log (id, tenant_id, user_id, entity_type, entity_id, action, before_state, after_state, created_at) VALUES ($1, $2, $3, 'ATTENDANCE_DAY_POLICY', $2, 'SCHEDULE', $4, $5, $6)",
        vec![Uuid::new_v4().into(), tenant_id.into(), claims.sub.into(), before.into(),
            audit_state(&state).into(), now.into()],
    )).await?;
    Ok(state)
}
