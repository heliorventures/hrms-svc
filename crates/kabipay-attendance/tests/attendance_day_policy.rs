use kabipay_attendance::attendance_day::*;
use kabipay_common::tenant_business_clock::TenantBusinessClock;
use chrono::{DateTime, NaiveDate, Utc};
use sea_orm::{entity::prelude::async_trait, Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement, TransactionTrait};
use std::{collections::{BTreeMap, VecDeque}, sync::{Arc, Mutex}};
use uuid::Uuid;
fn date(s: &str) -> NaiveDate { s.parse().unwrap() }
fn utc(s: &str) -> DateTime<Utc> { s.parse().unwrap() }
fn clock() -> TenantBusinessClock { TenantBusinessClock::from_name("Asia/Kolkata").unwrap() }
#[derive(Debug)]
struct Fixture { rows: Mutex<VecDeque<Vec<ProxyRow>>>, statements: Arc<Mutex<Vec<Statement>>>, fail_audit: bool }
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn query(&self, s: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.statements.lock().unwrap().push(s);
        Ok(self.rows.lock().unwrap().pop_front().expect("unexpected SQL query"))
    }
    async fn execute(&self, s: Statement) -> Result<ProxyExecResult, DbErr> {
        let fail = self.fail_audit && s.sql.starts_with("INSERT INTO audit_log");
        self.statements.lock().unwrap().push(s);
        if fail { return Err(DbErr::Custom("audit unavailable".into())); }
        Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
    }
}
async fn db(rows: Vec<Vec<ProxyRow>>) -> (sea_orm::DatabaseConnection, Arc<Mutex<Vec<Statement>>>) {
    db_with_audit_failure(rows, false).await
}
async fn db_with_audit_failure(rows: Vec<Vec<ProxyRow>>, fail_audit: bool) -> (sea_orm::DatabaseConnection, Arc<Mutex<Vec<Statement>>>) {
    let statements = Arc::new(Mutex::new(Vec::new()));
    let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(Fixture { rows: Mutex::new(rows.into()), statements: statements.clone(), fail_audit }))).await.unwrap();
    (db, statements)
}
fn exists(value: bool) -> Vec<ProxyRow> { vec![ProxyRow::new(BTreeMap::from([("has_attendance".into(), value.into())]))] }

fn missing_profile_snapshot(has_attendance: bool) -> Vec<ProxyRow> {
    vec![ProxyRow::new(BTreeMap::from([
        ("revision".into(), Option::<i64>::None.into()),
        ("legacy_activation_date".into(), Option::<NaiveDate>::None.into()),
        ("id".into(), Option::<Uuid>::None.into()),
        ("effective_work_date".into(), Option::<NaiveDate>::None.into()),
        ("boundary_minutes".into(), Option::<i32>::None.into()),
        ("timezone".into(), Option::<String>::None.into()),
        ("has_attendance".into(), has_attendance.into()),
    ]))]
}

/// The first statement reads its snapshot, then a fresh tenant's bootstrap commits
/// a 05:00 profile and first attendance row before any subsequent statement.
#[derive(Debug)]
struct BootstrapInterleaving {
    committed: Mutex<bool>,
    statements: Arc<Mutex<Vec<Statement>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for BootstrapInterleaving {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let includes_policy = statement.sql.contains("attendance_day_profile");
        let includes_attendance = statement.sql.contains("FROM attendance WHERE tenant_id");
        self.statements.lock().unwrap().push(statement);
        let mut committed = self.committed.lock().unwrap();
        let snapshot_committed = *committed;
        *committed = true;
        match (includes_policy, includes_attendance, snapshot_committed) {
            (true, _, true) => Ok(policy_rows(&[("0001-01-01", 300)], None)),
            (true, true, false) => Ok(missing_profile_snapshot(false)),
            (true, false, false) => Ok(vec![]),
            (false, true, committed) => Ok(exists(committed)),
            _ => Err(DbErr::Custom("unexpected bootstrap query".into())),
        }
    }

    async fn execute(&self, _statement: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom("policy reads must not write during bootstrap".into()))
    }
}

#[tokio::test]
async fn attendance_day_bootstrap_interleaving_keeps_one_consistent_read_snapshot() {
    // 03:00 local Sep12 belongs to Sep11 under BOTH consistent snapshots:
    // missing profile/no attendance (fresh default) and committed 05:00 profile.
    for initially_committed in [false, true] {
        let tenant = Uuid::new_v4();
        let statements = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(
            BootstrapInterleaving {
                committed: Mutex::new(initially_committed),
                statements: statements.clone(),
            },
        ))).await.unwrap();
        let now = utc("2026-09-11T21:30:00Z");
        let state = policy(&db, tenant, clock(), now).await.unwrap();
        let window = resolve_current_window(&state.versions, now).unwrap();
        assert_eq!(window.work_date, date("2026-09-11"));
        assert_eq!(window.starts_at, utc("2026-09-10T23:30:00Z"));
        assert_eq!(window.ends_at, utc("2026-09-11T23:30:00Z"));
        assert!(!state.legacy_activation_pending);
        assert_eq!(state.initialized, initially_committed);
        let statements = statements.lock().unwrap();
        assert_eq!(statements.len(), 1, "policy and bootstrap existence need one SQL snapshot");
        assert!(statements[0].to_string().contains(&tenant.to_string()));
        assert!(statements[0].sql.starts_with("SELECT"));
    }
}
#[tokio::test]
async fn attendance_day_missing_profile_reads_are_write_free_and_legacy_aware() {
    for (has_attendance, boundary, expected_start) in [(true, 0, "2026-09-10T18:30:00Z"), (false, 300, "2026-09-10T23:30:00Z")] {
        let tenant = Uuid::new_v4();
        let (db, statements) = db(vec![missing_profile_snapshot(has_attendance)]).await;
        let policy = policy(&db, tenant, clock(), utc("2026-09-11T10:00:00Z")).await.unwrap();
        assert!(!policy.initialized);
        assert_eq!(policy.legacy_activation_pending, has_attendance);
        assert_eq!(policy.revision, 0);
        let w = resolve_window(&policy.versions, date("2026-09-11")).unwrap();
        assert_eq!(w.boundary_minutes, boundary);
        assert_eq!(w.starts_at, utc(expected_start));
        for statement in statements.lock().unwrap().iter() {
            assert!(statement.sql.trim_start().starts_with("SELECT"));
            assert!(statement.to_string().contains(&tenant.to_string()), "tenant must qualify every read");
        }
    }
}
fn frozen(tenant: Uuid) -> Vec<ProxyRow> {
    vec![ProxyRow::new(BTreeMap::from([
        ("tenant_id".into(), tenant.into()),
        ("work_date".into(), date("2026-09-11").into()),
        ("starts_at".into(), utc("2026-09-10T23:30:00Z").into()),
        ("ends_at".into(), utc("2026-09-11T23:30:00Z").into()),
        ("timezone".into(), "Asia/Kolkata".into()),
        ("boundary_minutes".into(), 300.into()),
        ("policy_version_id".into(), Uuid::new_v4().into()),
    ]))]
}
#[tokio::test]
async fn attendance_day_frozen_history_ignores_later_timezone() {
    let tenant = Uuid::new_v4();
    let (db, statements) = db(vec![frozen(tenant)]).await;
    let w = window_for_date(&db, tenant, TenantBusinessClock::from_name("America/New_York").unwrap(), date("2026-09-11"), utc("2026-10-01T10:00:00Z")).await.unwrap();
    assert_eq!(w.starts_at, utc("2026-09-10T23:30:00Z"));
    assert_eq!(w.timezone, "Asia/Kolkata");
    assert_eq!(statements.lock().unwrap().len(), 1);
}
#[tokio::test]
async fn attendance_day_freeze_existing_window_is_idempotent_and_locks_tenant() {
    let tenant = Uuid::new_v4();
    let (db, statements) = db(vec![frozen(tenant)]).await;
    let tx = db.begin().await.unwrap();
    let w = ensure_window(&tx, tenant, clock(), date("2026-09-11"), utc("2026-09-11T10:00:00Z")).await.unwrap();
    assert_eq!(w.ends_at, utc("2026-09-11T23:30:00Z"));
    let sql = statements.lock().unwrap();
    assert!(sql[0].sql.contains("pg_advisory_xact_lock"));
    assert!(sql.iter().all(|s| !s.sql.starts_with("INSERT")));
    assert!(sql.iter().all(|s| s.to_string().contains(&tenant.to_string())));
}

fn claims(tenant: Uuid, scope: &str) -> kabipay_common::context::ClientClaims {
    serde_json::from_value(serde_json::json!({"sub":Uuid::new_v4(),"iss":"kabipay-client","exp":0,"iat":0,"tenant_id":tenant,"permissions":["attendance:punch_policy"],"permission_scopes":{"attendance:punch_policy":scope}})).unwrap()
}
fn command(revision: i64, effective: &str) -> SchedulePolicyCommand {
    SchedulePolicyCommand { expected_revision: revision, effective_work_date: date(effective), boundary_minutes: 360 }
}

#[tokio::test]
async fn attendance_day_preview_validates_authority_revision_and_exact_transition_without_writes() {
    let tenant = Uuid::new_v4();
    let (denied_db, denied_statements) = db(vec![]).await;
    let denied = preview_policy(&denied_db, tenant, clock(), &claims(tenant, "SELF"), command(7, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await;
    assert!(matches!(denied, Err(kabipay_common::KabiPayError::Forbidden(_))));
    assert!(denied_statements.lock().unwrap().is_empty());
    let (preview_db, statements) = db(vec![policy_rows(&[("0001-01-01", 300)], None), vec![]]).await;
    let preview = preview_policy(&preview_db, tenant, clock(), &claims(tenant, "ALL"), command(7, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await.unwrap();
    assert_eq!(preview.revision, 7);
    assert_eq!(preview.transition.starts_at, utc("2026-09-11T23:30:00Z"));
    assert_eq!(preview.transition.ends_at, utc("2026-09-13T00:30:00Z"));
    assert_eq!(preview.following.starts_at, utc("2026-09-13T00:30:00Z"));
    assert!(statements.lock().unwrap().iter().all(|s| s.sql.starts_with("SELECT")));
    let (stale_db, _) = db(vec![policy_rows(&[("0001-01-01", 300)], None)]).await;
    assert!(matches!(preview_policy(&stale_db, tenant, clock(), &claims(tenant, "ALL"), command(6, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await, Err(kabipay_common::KabiPayError::Conflict(_))));
}
fn policy_rows(versions: &[(&str, i32)], activation: Option<NaiveDate>) -> Vec<ProxyRow> {
    policy_rows_at_revision(versions, activation, 7)
}
fn policy_rows_at_revision(versions: &[(&str, i32)], activation: Option<NaiveDate>, revision: i64) -> Vec<ProxyRow> {
    versions.iter().map(|(effective, minutes)| ProxyRow::new(BTreeMap::from([
        ("revision".into(), revision.into()),
        ("legacy_activation_date".into(), activation.into()),
        ("id".into(), Uuid::new_v4().into()),
        ("effective_work_date".into(), date(effective).into()),
        ("boundary_minutes".into(), (*minutes).into()),
        ("timezone".into(), "Asia/Kolkata".into()),
        ("has_attendance".into(), true.into()),
    ]))).collect()
}

#[derive(Clone, Copy, Debug)]
enum AgreementFixture { Fresh, Legacy, PendingLegacy, PendingCustom }
impl AgreementFixture {
    fn snapshot(self) -> Vec<ProxyRow> {
        match self {
            Self::Fresh => missing_profile_snapshot(false),
            Self::Legacy => missing_profile_snapshot(true),
            Self::PendingLegacy => policy_rows(&[("0001-01-01", 0), ("2026-09-12", 300)], Some(date("2026-09-12"))),
            Self::PendingCustom => policy_rows(&[("0001-01-01", 300), ("2026-09-13", 240)], None),
        }
    }
    fn revision(self) -> i64 {
        match self { Self::Fresh | Self::Legacy => 0, _ => 7 }
    }
}

#[tokio::test]
async fn attendance_day_preview_schedule_agreement_preserves_bootstrap_and_replacement_intervals() {
    for (fixture, effective, expected_start, expected_end, expected_anchor) in [
        (AgreementFixture::Fresh, "2026-09-12", "2026-09-11T23:30:00Z", "2026-09-13T00:30:00Z", None),
        (AgreementFixture::Legacy, "2026-09-13", "2026-09-12T18:30:00Z", "2026-09-14T00:30:00Z", Some("2026-09-12")),
        (AgreementFixture::PendingLegacy, "2026-09-13", "2026-09-12T18:30:00Z", "2026-09-14T00:30:00Z", Some("2026-09-12")),
        (AgreementFixture::PendingCustom, "2026-09-14", "2026-09-13T23:30:00Z", "2026-09-15T00:30:00Z", None),
    ] {
        let tenant = Uuid::new_v4();
        let now = utc("2026-09-11T10:00:00Z");
        let cmd = command(fixture.revision(), effective);
        let (preview_db, read_statements) = db(vec![fixture.snapshot(), vec![]]).await;
        let preview = preview_policy(&preview_db, tenant, clock(), &claims(tenant, "ALL"), cmd.clone(), now).await.unwrap();
        let (schedule_db, write_statements) = db(vec![fixture.snapshot(), vec![]]).await;
        let txn = schedule_db.begin().await.unwrap();
        let scheduled = schedule_policy(&txn, tenant, clock(), &claims(tenant, "ALL"), cmd, now).await.unwrap();
        let actual = resolve_window(&scheduled.versions, date(effective)).unwrap();
        assert_eq!(preview.transition.starts_at, utc(expected_start), "{fixture:?}");
        assert_eq!(preview.transition.ends_at, utc(expected_end), "{fixture:?}");
        assert_eq!(actual.starts_at, utc(expected_start), "{fixture:?}");
        assert_eq!(actual.ends_at, utc(expected_end), "{fixture:?}");
        assert_eq!(preview.following.starts_at, utc(expected_end));
        assert_eq!(scheduled.legacy_activation_date, expected_anchor.map(date));
        assert_eq!(preview.revision, fixture.revision());
        assert_eq!(scheduled.revision, if fixture.revision() == 0 { 2 } else { 8 });
        assert_eq!(scheduled.versions.len(), 2);
        assert!(read_statements.lock().unwrap().iter().all(|s| s.sql.starts_with("SELECT")));
        let statements = write_statements.lock().unwrap();
        assert!(statements[0].sql.contains("pg_advisory_xact_lock"));
        assert!(statements.iter().any(|s| s.sql.starts_with("INSERT INTO audit_log")));
    }
}

#[tokio::test]
async fn attendance_day_preview_schedule_agreement_rejects_stale_active_and_affected_frozen_dates() {
    for (fixture, effective, affected) in [
        (AgreementFixture::Fresh, "2026-09-13", "2026-09-13"),
        (AgreementFixture::Legacy, "2026-09-13", "2026-09-12"),
        (AgreementFixture::PendingLegacy, "2026-09-13", "2026-09-12"),
        (AgreementFixture::PendingCustom, "2026-09-14", "2026-09-13"),
    ] {
        for rejection in ["revision", "active", "frozen"] {
            let tenant = Uuid::new_v4();
            let mut cmd = command(fixture.revision(), effective);
            if rejection == "revision" { cmd.expected_revision -= 1; }
            if rejection == "active" { cmd.effective_work_date = date("2026-09-11"); }
            let expected_code = if rejection == "active" { "VALIDATION_ERROR" } else { "CONFLICT" };
            for schedule in [false, true] {
                let mut replies = vec![fixture.snapshot()];
                if rejection == "frozen" { replies.push(vec![ProxyRow::new(BTreeMap::from([("work_date".into(), date(affected).into())]))]); }
                let (db, statements) = db(replies).await;
                let now = utc("2026-09-11T10:00:00Z");
                let error = if schedule {
                    let txn = db.begin().await.unwrap();
                    let error = schedule_policy(&txn, tenant, clock(), &claims(tenant, "ALL"), cmd.clone(), now).await.unwrap_err();
                    txn.rollback().await.unwrap();
                    error
                } else {
                    preview_policy(&db, tenant, clock(), &claims(tenant, "ALL"), cmd.clone(), now).await.unwrap_err()
                };
                assert_eq!(error.code(), expected_code, "{fixture:?} {rejection} schedule={schedule}");
                let statements = statements.lock().unwrap();
                if !schedule { assert!(statements.iter().all(|s| s.sql.starts_with("SELECT"))); }
                if rejection == "frozen" {
                    let frozen = statements.iter().find(|s| s.sql.contains("work_date >= $2")).unwrap();
                    assert!(frozen.to_string().contains(&format!("'{affected}'")), "{fixture:?}: {frozen}");
                }
            }
        }
    }
}

#[tokio::test]
async fn attendance_day_preview_schedule_agreement_rejects_exhausted_revision() {
    let tenant = Uuid::new_v4();
    for schedule in [false, true] {
        let (db, _) = db(vec![policy_rows_at_revision(&[("0001-01-01", 300)], None, i64::MAX), vec![]]).await;
        let now = utc("2026-09-11T10:00:00Z");
        let cmd = command(i64::MAX, "2026-09-12");
        let rejected = if schedule {
            let txn = db.begin().await.unwrap();
            schedule_policy(&txn, tenant, clock(), &claims(tenant, "ALL"), cmd, now).await.is_err()
        } else {
            preview_policy(&db, tenant, clock(), &claims(tenant, "ALL"), cmd, now).await.is_err()
        };
        assert!(rejected, "preview and save must reject an exhausted revision; schedule={schedule}");
    }
}
#[tokio::test]
async fn attendance_day_policy_rejects_unauthorized_and_foreign_tenants_before_sql() {
    let tenant = Uuid::new_v4();
    for claimant in [claims(tenant, "TEAM"), claims(Uuid::new_v4(), "ALL")] {
        let (db, statements) = db(vec![]).await;
        let tx = db.begin().await.unwrap();
        let err = schedule_policy(&tx, tenant, clock(), &claimant, command(7, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await.unwrap_err();
        assert!(matches!(err, kabipay_common::KabiPayError::Forbidden(_)));
        assert!(statements.lock().unwrap().is_empty());
    }
}
#[tokio::test]
async fn attendance_day_policy_stale_revision_and_active_day_rejected_without_writes() {
    for cmd in [command(6, "2026-09-12"), command(7, "2026-09-11"), command(7, "2026-09-10")] {
        let tenant = Uuid::new_v4();
        let (db, statements) = db(vec![policy_rows(&[("0001-01-01", 300)], None)]).await;
        let tx = db.begin().await.unwrap();
        let err = schedule_policy(&tx, tenant, clock(), &claims(tenant, "ALL"), cmd.clone(), utc("2026-09-11T10:00:00Z")).await.unwrap_err();
        if cmd.expected_revision == 6 { assert!(matches!(err, kabipay_common::KabiPayError::Conflict(_))); }
        else { assert!(matches!(err, kabipay_common::KabiPayError::Validation(_))); }
        assert!(statements.lock().unwrap().iter().all(|s| !s.sql.starts_with("INSERT") && !s.sql.starts_with("UPDATE")));
    }
}
#[tokio::test]
async fn attendance_day_policy_frozen_future_rejects_replacement() {
    let tenant = Uuid::new_v4();
    let (db, statements) = db(vec![policy_rows(&[("0001-01-01", 300), ("2026-09-13", 240)], None), vec![ProxyRow::new(BTreeMap::from([("work_date".into(), date("2026-09-13").into())]))]]).await;
    let tx = db.begin().await.unwrap();
    let err = schedule_policy(&tx, tenant, clock(), &claims(tenant, "ALL"), command(7, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await.unwrap_err();
    assert!(matches!(err, kabipay_common::KabiPayError::Conflict(_)));
    assert!(statements.lock().unwrap().iter().all(|s| !s.sql.starts_with("INSERT") && !s.sql.starts_with("UPDATE")));
}
#[tokio::test]
async fn attendance_day_policy_replacement_audits_and_preserves_history() {
    let tenant = Uuid::new_v4();
    let claimant = claims(tenant, "ALL");
    let (db, statements) = db(vec![policy_rows(&[("0001-01-01", 300), ("2026-09-13", 240)], None), vec![]]).await;
    let tx = db.begin().await.unwrap();
    let result = schedule_policy(&tx, tenant, clock(), &claimant, command(7, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await.unwrap();
    assert_eq!(result.revision, 8);
    assert_eq!(result.versions.len(), 2);
    assert_eq!(result.versions[0].boundary_minutes, 300);
    assert_eq!(result.versions[1].effective_work_date, date("2026-09-12"));
    assert_eq!(resolve_window(&result.versions, date("2026-09-12")).unwrap().ends_at, utc("2026-09-13T00:30:00Z"));
    let sql = statements.lock().unwrap();
    assert!(sql.iter().all(|s| s.to_string().contains(&tenant.to_string())));
    assert!(!sql.iter().any(|s| s.sql.starts_with("DELETE")));
    let audit = sql.iter().find(|s| s.sql.starts_with("INSERT INTO audit_log")).expect("same-transaction policy audit");
    assert!(audit.to_string().contains(&claimant.sub.to_string()));
    assert!(audit.to_string().contains("before_state") && audit.to_string().contains("after_state"));
}
#[tokio::test]
async fn attendance_day_initialization_preserves_legacy_active_day_and_freezes_explicit_transition() {
    for (work_date, start, end) in [
        ("2026-09-11", "2026-09-10T18:30:00Z", "2026-09-11T18:30:00Z"),
        ("2026-09-12", "2026-09-11T18:30:00Z", "2026-09-12T23:30:00Z")
    ] {
        let tenant = Uuid::new_v4();
        let (db, statements) = db(vec![vec![], missing_profile_snapshot(true), vec![]]).await;
        let tx = db.begin().await.unwrap();
        let result = ensure_window(&tx, tenant, clock(), date(work_date), utc("2026-09-11T10:00:00Z")).await.unwrap();
        assert_eq!(result.starts_at, utc(start)); assert_eq!(result.ends_at, utc(end));
        let sql = statements.lock().unwrap();
        assert!(sql.iter().all(|s| s.to_string().contains(&tenant.to_string())));
        let profile = sql.iter().find(|s| s.sql.starts_with("INSERT INTO attendance_day_profile")).unwrap();
        assert!(profile.to_string().contains("2026-09-12"));
        assert_eq!(sql.iter().filter(|s| s.sql.starts_with("INSERT INTO attendance_day_policy_version")).count(), 2);
        assert!(sql.iter().any(|s| s.sql.starts_with("INSERT INTO attendance_day_window")));
    }
}
#[tokio::test]
async fn attendance_day_initialization_fresh_tenant_uses_five_without_conversion() {
    let tenant = Uuid::new_v4();
    let (db, statements) = db(vec![vec![], missing_profile_snapshot(false), vec![]]).await;
    let tx = db.begin().await.unwrap();
    let result = ensure_window(&tx, tenant, clock(), date("2026-09-11"), utc("2026-09-11T10:00:00Z")).await.unwrap();
    assert_eq!(result.starts_at, utc("2026-09-10T23:30:00Z"));
    assert_eq!(result.ends_at, utc("2026-09-11T23:30:00Z"));
    assert_eq!(statements.lock().unwrap().iter().filter(|s| s.sql.starts_with("INSERT INTO attendance_day_policy_version")).count(), 1);
}
#[tokio::test]
async fn attendance_day_current_read_uses_frozen_interval_and_exclusive_end_query() {
    let tenant = Uuid::new_v4();
    let (db, statements) = db(vec![frozen(tenant)]).await;
    let now = utc("2026-09-11T23:29:59Z");
    let result = current_window(&db, tenant, clock(), now).await.unwrap();
    assert_eq!(result.work_date, date("2026-09-11"));
    let sql = statements.lock().unwrap();
    assert_eq!(sql.len(), 1);
    assert!(sql[0].sql.contains("starts_at <= $2") && sql[0].sql.contains("ends_at > $2"));
    assert!(sql[0].to_string().contains(&tenant.to_string()));
}
#[tokio::test]
async fn attendance_day_current_read_after_end_resolves_next_window_without_writes() {
    let tenant = Uuid::new_v4();
    let (db, statements) = db(vec![vec![], policy_rows(&[("0001-01-01", 300)], None)]).await;
    let result = current_window(&db, tenant, clock(), utc("2026-09-11T23:30:00Z")).await.unwrap();
    assert_eq!(result.work_date, date("2026-09-12"));
    assert_eq!(result.starts_at, utc("2026-09-11T23:30:00Z"));
    assert!(statements.lock().unwrap().iter().all(|s| s.sql.starts_with("SELECT")));
}
#[tokio::test]
async fn attendance_day_policy_audit_failure_propagates_for_transaction_rollback() {
    let tenant = Uuid::new_v4();
    let (db, _) = db_with_audit_failure(vec![policy_rows(&[("0001-01-01", 300)], None), vec![]], true).await;
    let tx = db.begin().await.unwrap();
    let err = schedule_policy(&tx, tenant, clock(), &claims(tenant, "ALL"), command(7, "2026-09-12"), utc("2026-09-11T10:00:00Z")).await.unwrap_err();
    assert!(matches!(err, kabipay_common::KabiPayError::Database(_)));
    tx.rollback().await.unwrap();
}
#[tokio::test]
async fn attendance_day_pending_legacy_activation_can_be_delayed_without_moving_original_anchor() {
    let tenant = Uuid::new_v4();
    let (db, _) = db(vec![policy_rows(&[("0001-01-01", 0), ("2026-09-12", 300)], Some(date("2026-09-12"))), vec![]]).await;
    let tx = db.begin().await.unwrap();
    let result = schedule_policy(&tx, tenant, clock(), &claims(tenant, "ALL"), command(7, "2026-09-13"), utc("2026-09-11T10:00:00Z")).await.unwrap();
    assert_eq!(result.legacy_activation_date, Some(date("2026-09-12")));
    assert!(result.legacy_activation_pending);
    assert_eq!(result.versions.last().unwrap().effective_work_date, date("2026-09-13"));
    let still_legacy = resolve_window(&result.versions, date("2026-09-12")).unwrap();
    assert_eq!(still_legacy.starts_at, utc("2026-09-11T18:30:00Z"));
    assert_eq!(still_legacy.ends_at, utc("2026-09-12T18:30:00Z"));
    let transition = resolve_window(&result.versions, date("2026-09-13")).unwrap();
    assert_eq!(transition.starts_at, utc("2026-09-12T18:30:00Z"));
    assert_eq!(transition.ends_at, utc("2026-09-14T00:30:00Z"));
}
