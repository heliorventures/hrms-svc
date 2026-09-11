//! SQL boundary tests execute the real punch/expiry services without PostgreSQL.
use super::*;
use super::tests::{row, utc, window};
use sea_orm::{entity::prelude::async_trait, Database, DbErr, Iden, Iterable, ModelTrait, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Value};
use std::{collections::BTreeMap, sync::{Arc, Mutex}};

#[derive(Debug)]
struct Fixture {
    rows: Mutex<Vec<BTreeMap<String, Value>>>,
    statements: Mutex<Vec<Statement>>,
    now: Mutex<DateTime<Utc>>,
    advance_on_lock: bool,
    fail_audit: bool,
    correct_on_lock: bool,
}
fn fields(model: attendance::Model) -> BTreeMap<String, Value> {
    attendance::Column::iter().map(|c| (c.to_string(), model.get(c))).collect()
}
fn value_date(value: &Value) -> NaiveDate { match value { Value::ChronoDate(Some(v)) => **v, _ => panic!("expected date") } }
fn value_uuid(value: &Value) -> Uuid { match value { Value::Uuid(Some(v)) => **v, _ => panic!("expected UUID") } }
fn value_text(value: &Value) -> Option<&str> { match value { Value::String(Some(v)) => Some(v), _ => None } }
impl Fixture {
    fn new(advance_on_lock: bool) -> Self {
        Self { rows: Mutex::new(vec![fields(row())]), statements: Mutex::new(vec![]),
            now: Mutex::new(utc("2026-09-11T23:29:59Z")), advance_on_lock, fail_audit: false, correct_on_lock: false }
    }
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.statements.lock().unwrap().push(statement.clone());
        if sql.contains("attendance_punch_policy") { return Ok(vec![]); }
        if sql.starts_with("SELECT work_date, starts_at") {
            let date: NaiveDate = if sql.contains("2026-09-12") || sql.contains("2026-09-11 23:30:00") {
                "2026-09-12".parse().unwrap()
            } else { "2026-09-11".parse().unwrap() };
            let w = window();
            let offset = if date == w.work_date { chrono::Duration::zero() } else { chrono::Duration::days(1) };
            return Ok(vec![ProxyRow::new(BTreeMap::from([
                ("work_date".into(), date.into()), ("starts_at".into(), (w.starts_at+offset).into()),
                ("ends_at".into(), (w.ends_at+offset).into()), ("timezone".into(), w.timezone.into()),
                ("boundary_minutes".into(), 300.into()), ("policy_version_id".into(), w.policy_version_id.into()),
            ]))]);
        }
        if sql.starts_with("INSERT INTO \"attendance\"") {
            let columns = statement.sql.split_once('(').unwrap().1.split_once(')').unwrap().0;
            let values = &statement.values.as_ref().unwrap().0;
            let inserted: BTreeMap<_, _> = columns.split(',').map(|s| s.trim().trim_matches('"').to_owned())
                .zip(values.iter().cloned()).collect();
            self.rows.lock().unwrap().push(inserted.clone());
            return Ok(vec![ProxyRow::new(inserted)]);
        }
        if sql.starts_with("UPDATE \"attendance\" SET") {
            let values = &statement.values.as_ref().unwrap().0;
            let mut rows = self.rows.lock().unwrap();
            let target = rows.iter_mut().find(|row| sql.contains(&value_uuid(&row["id"]).to_string()))
                .ok_or_else(|| DbErr::Custom("attendance update target missing".into()))?;
            let assignments = statement.sql.split_once(" SET ").unwrap().1.split_once(" WHERE ").unwrap().0;
            for assignment in assignments.split(',') {
                let (column, parameter) = assignment.split_once('=').unwrap();
                let index: usize = parameter.trim().trim_start_matches('$').parse().unwrap();
                target.insert(column.trim().trim_matches('"').into(), values[index - 1].clone());
            }
            return Ok(vec![ProxyRow::new(target.clone())]);
        }
        if sql.contains("FROM \"attendance\"") {
            let rows = self.rows.lock().unwrap();
            return Ok(rows.iter().filter(|r| {
                let date = value_date(&r["work_date"]);
                let date_match = if sql.contains("\"work_date\" =") { sql.contains(&format!("\"work_date\" = '{date}'")) }
                    else if sql.contains("\"work_date\" <") { date < "2026-09-12".parse().unwrap() } else { true };
                let status_match = !sql.contains("\"status\" = 'OPEN'") || value_text(&r["status"]) == Some("OPEN");
                let id_match = !sql.contains("\"id\" =") || sql.contains(&value_uuid(&r["id"]).to_string());
                date_match && status_match && id_match
            }).cloned().map(ProxyRow::new).collect());
        }
        Err(DbErr::Custom(format!("unexpected SQL: {sql}")))
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        let sql = statement.to_string();
        self.statements.lock().unwrap().push(statement.clone());
        if sql.contains("pg_advisory_xact_lock") {
            if self.advance_on_lock && sql.contains("attendance:") { *self.now.lock().unwrap() = utc("2026-09-11T23:30:00Z"); }
            // A correction commits while expiry waits for the policy lock,
            // after the worker's initial candidate snapshot but before its
            // locked read. No compliant writer runs inside the held lock.
            if self.correct_on_lock && sql.contains("attendance-day-policy:") {
                let mut rows = self.rows.lock().unwrap();
                rows[0].insert("status".into(), "COMPLETE".into());
                rows[0].insert("check_out_at".into(), utc("2026-09-11T22:30:00Z").into());
                rows[0].insert("check_out_time".into(), "04:00:00".parse::<chrono::NaiveTime>().unwrap().into());
            }
        } else if sql.starts_with("UPDATE attendance SET status") || sql.starts_with("UPDATE \"attendance\" SET \"status\"") {
            for row in self.rows.lock().unwrap().iter_mut().filter(|r| value_text(&r["status"]) == Some("OPEN")) {
                row.insert("status".into(), "INCOMPLETE".into());
                row.insert("regularization_status".into(), "MISSED_PUNCH_OUT".into());
            }
        } else if sql.starts_with("INSERT INTO audit_log") {
            if self.fail_audit { return Err(DbErr::Custom("audit unavailable".into())); }
        } else { return Err(DbErr::Custom(format!("unexpected execution: {sql}"))); }
        Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
    }
}
async fn fixture_db(fixture: Arc<Fixture>) -> DatabaseConnection {
    Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(SharedFixture(fixture)))).await.unwrap()
}
// Arc delegates only the external SQL boundary; all attendance logic is real.
#[derive(Debug)]
struct SharedFixture(Arc<Fixture>);
#[async_trait::async_trait]
impl ProxyDatabaseTrait for SharedFixture {
    async fn query(&self, s: Statement) -> Result<Vec<ProxyRow>, DbErr> { self.0.query(s).await }
    async fn execute(&self, s: Statement) -> Result<ProxyExecResult, DbErr> { self.0.execute(s).await }
}
#[tokio::test]
async fn attendance_day_punch_resamples_after_locks_expires_original_and_opens_new_day() {
    let fixture = Arc::new(Fixture::new(true));
    let db = fixture_db(fixture.clone()).await;
    let result = crate::services::attendance_service::punch_today_with_clock(
        &db, Uuid::from_u128(2), Uuid::from_u128(4), TenantBusinessClock::from_name("Asia/Kolkata").unwrap(),
        None, None, || *fixture.now.lock().unwrap(),
    ).await.unwrap();
    assert_eq!(result.check_in_at, Some(utc("2026-09-11T23:30:00Z")));
    assert_eq!(result.work_date, "2026-09-12".parse::<NaiveDate>().unwrap());
    assert_eq!(result.status.as_deref(), Some("OPEN"));
    let rows = fixture.rows.lock().unwrap();
    assert_eq!(rows.len(), 2);
    assert_eq!(value_text(&rows[0]["status"]), Some("INCOMPLETE"));
    assert!(matches!(&rows[0]["check_out_time"], Value::ChronoTime(None)));
    assert!(matches!(&rows[0]["check_out_at"], Value::ChronoDateTimeUtc(None)));
    assert_eq!(rows.iter().filter(|r| value_text(&r["status"]) == Some("OPEN")).count(), 1);
    let statements = fixture.statements.lock().unwrap();
    let locks: Vec<_> = statements.iter().filter(|s| s.sql.contains("pg_advisory_xact_lock")).collect();
    assert!(locks[0].to_string().contains("attendance-day-policy:"));
    assert!(statements.iter().any(|s| s.sql.contains("INSERT INTO audit_log")));
}

#[tokio::test]
async fn attendance_day_worker_retires_once_and_preserves_a_correction_seen_after_lock() {
    for corrected in [false, true] {
        let mut state = Fixture::new(true);
        state.correct_on_lock = corrected;
        let fixture = Arc::new(state);
        let db = fixture_db(fixture.clone()).await;
        let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
        let first = sweep_with_clock(&db, Uuid::from_u128(2), clock, 25, || *fixture.now.lock().unwrap()).await.unwrap();
        assert_eq!(first.expired, if corrected { 0 } else { 1 });
        assert_eq!(first.failed, 0);
        let second = sweep_with_clock(&db, Uuid::from_u128(2), clock, 25, || *fixture.now.lock().unwrap()).await.unwrap();
        assert_eq!(second.expired, 0);
        let rows = fixture.rows.lock().unwrap();
        assert_eq!(value_text(&rows[0]["status"]), Some(if corrected { "COMPLETE" } else { "INCOMPLETE" }));
        if corrected { assert_eq!(rows[0]["check_out_at"], utc("2026-09-11T22:30:00Z").into()); }
        else { assert!(matches!(rows[0]["check_out_at"], Value::ChronoDateTimeUtc(None))); }
        let statements = fixture.statements.lock().unwrap();
        assert_eq!(statements.iter().filter(|s| s.sql.starts_with("INSERT INTO audit_log")).count(), if corrected { 0 } else { 1 });
    }
}

#[tokio::test]
async fn attendance_day_punch_before_cutoff_completes_the_existing_original_day() {
    let fixture = Arc::new(Fixture::new(false));
    let db = fixture_db(fixture.clone()).await;
    let result = crate::services::attendance_service::punch_today_with_clock(
        &db, Uuid::from_u128(2), Uuid::from_u128(4), TenantBusinessClock::from_name("Asia/Kolkata").unwrap(),
        None, None, || *fixture.now.lock().unwrap(),
    ).await.unwrap();
    assert_eq!(result.id, Uuid::from_u128(1));
    assert_eq!(result.work_date, "2026-09-11".parse::<NaiveDate>().unwrap());
    assert_eq!(result.check_out_at, Some(utc("2026-09-11T23:29:59Z")));
    assert_eq!(result.status.as_deref(), Some("COMPLETE"));
    assert_eq!(fixture.rows.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn attendance_day_read_projection_expires_without_writes_or_revision_changes() {
    let fixture = Arc::new(Fixture::new(false));
    let db = fixture_db(fixture.clone()).await;
    let mut rows = vec![row()];
    project_rows(&db, Uuid::from_u128(2), TenantBusinessClock::from_name("Asia/Kolkata").unwrap(), &mut rows, utc("2026-09-11T23:30:00Z")).await.unwrap();
    assert_eq!(rows[0].status.as_deref(), Some("INCOMPLETE"));
    assert!(rows[0].check_out_at.is_none() && rows[0].check_out_time.is_none());
    assert_eq!(rows[0].updated_at, utc("2026-09-11T17:30:00Z"));
    assert!(fixture.statements.lock().unwrap().iter().all(|s| s.sql.starts_with("SELECT")));
}

#[tokio::test]
async fn attendance_day_worker_reports_audit_failure_for_retry() {
    let mut state = Fixture::new(true);
    state.fail_audit = true;
    let fixture = Arc::new(state);
    let db = fixture_db(fixture.clone()).await;
    let result = sweep_with_clock(&db, Uuid::from_u128(2), TenantBusinessClock::from_name("Asia/Kolkata").unwrap(), 25, || *fixture.now.lock().unwrap()).await.unwrap();
    assert_eq!(result.failed, 1);
    assert_eq!(result.expired, 0);
}
