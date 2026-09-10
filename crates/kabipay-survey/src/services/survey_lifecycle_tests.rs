use std::{collections::BTreeMap, sync::{Arc, Mutex}};

use chrono::{DateTime, Duration, Utc};
use sea_orm::{entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
use uuid::Uuid;

use super::survey_lifecycle::{process_survey_with_clock, record_transition_failure, open_survey_with_clock, ManagementAction};

#[derive(Clone, Debug)]
struct Fixture {
    tenant: Uuid,
    survey: Uuid,
    closes_at: Option<DateTime<Utc>>,
    opens_at: Option<DateTime<Utc>>,
    opened: bool,
    manually_opened: bool,
    failed: bool,
    fail_update: bool,
    sql: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new() -> Self {
        Self { tenant: Uuid::new_v4(), survey: Uuid::new_v4(), closes_at: None, opens_at: None,
            opened: false, manually_opened: false, failed: false, fail_update: false, sql: Arc::default() }
    }
    async fn db(&self) -> DatabaseConnection {
        Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(self.clone()))).await.unwrap()
    }
    fn statements(&self) -> Vec<String> { self.sql.lock().unwrap().clone() }
    fn push(&self, sql: impl Into<String>) { self.sql.lock().unwrap().push(sql.into()); }
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn begin(&self) { self.push("BEGIN"); }
    async fn commit(&self) { self.push("COMMIT"); }
    async fn rollback(&self) { self.push("ROLLBACK"); }
    fn start_rollback(&self) { self.push("ROLLBACK"); }
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.push(sql.clone());
        if sql.contains("FROM \"survey\"") {
            assert!(sql.contains(&self.tenant.to_string()));
            assert!(sql.contains(&self.survey.to_string()));
            assert!(sql.contains("FOR UPDATE"));
            let now = Utc::now();
            return Ok(vec![ProxyRow::new(BTreeMap::from([
                ("id".into(), self.survey.into()), ("tenant_id".into(), self.tenant.into()),
                ("title".into(), "Pulse".into()), ("description".into(), Option::<String>::None.into()),
                ("status".into(), "PUBLISHED".into()), ("opens_at".into(), self.opens_at.into()),
                ("closes_at".into(), self.closes_at.into()), ("minimum_report_group_size".into(), 3i32.into()),
                ("created_by".into(), Uuid::new_v4().into()), ("published_at".into(), Some(now).into()),
                ("closed_at".into(), Option::<DateTime<Utc>>::None.into()),
                ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
            ]))]);
        }
        if sql.contains("FROM \"audit_log\"") {
            assert!(sql.contains(&self.tenant.to_string()));
            assert!(sql.contains(&self.survey.to_string()));
            let action = if self.manually_opened {
                assert!(sql.contains("SURVEY_MANUALLY_OPENED"), "manual opening must count as already opened");
                Some("SURVEY_MANUALLY_OPENED")
            } else if self.failed { Some("SURVEY_TRANSITION_FAILED") }
                else if self.opened { Some("SURVEY_OPENED") } else { None };
            return Ok(action.map(|value| vec![ProxyRow::new(BTreeMap::from([("action".into(), value.into())]))]).unwrap_or_default());
        }
        Err(DbErr::Custom(format!("unexpected lifecycle query: {sql}")))
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        let sql = statement.to_string();
        self.push(sql.clone());
        if sql.starts_with("UPDATE \"survey\"") && self.fail_update {
            return Err(DbErr::Custom("simulated transition update error".into()));
        }
        assert!(sql.starts_with("UPDATE \"survey\"") || sql.starts_with("INSERT INTO \"audit_log\""));
        assert!(sql.contains(&self.tenant.to_string()));
        Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
    }
}

#[tokio::test]
async fn manual_open_uses_post_lock_clock_and_never_rebuilds_assignments() {
    let now = Utc::now();
    let mut fixture = Fixture::new();
    fixture.opens_at = Some(now + Duration::hours(1));
    let db = fixture.db().await;
    open_survey_with_clock(&db, fixture.tenant, fixture.survey, Uuid::new_v4(), || {
        assert!(fixture.statements().iter().any(|sql| sql.contains("FOR UPDATE")));
        now
    }).await.unwrap();
    let sql = fixture.statements();
    assert!(sql.iter().any(|s| s.starts_with("UPDATE \"survey\"") && s.contains("\"opens_at\"")));
    assert!(sql.iter().any(|s| s.starts_with("INSERT INTO \"audit_log\"") && s.contains("SURVEY_MANUALLY_OPENED")));
    assert!(!sql.iter().any(|s| s.contains("survey_assignment") || s.contains("survey_response") || s.contains("survey_question")));
    assert_eq!(sql.last().unwrap(), "COMMIT");
}

#[tokio::test]
async fn due_close_uses_clock_after_survey_lock_and_commits_metadata_only_audit() {
    let now = Utc::now();
    let mut fixture = Fixture::new();
    fixture.closes_at = Some(now);
    let db = fixture.db().await;
    let action = process_survey_with_clock(&db, fixture.tenant, fixture.survey, || {
        assert!(fixture.statements().iter().any(|sql| sql.contains("FOR UPDATE")));
        now
    }).await.unwrap();
    assert_eq!(action, Some(ManagementAction::AutomaticallyClosed));
    let sql = fixture.statements();
    let update = sql.iter().position(|s| s.starts_with("UPDATE")).unwrap();
    let audit = sql.iter().position(|s| s.starts_with("INSERT INTO \"audit_log\"")).unwrap();
    assert!(update < audit);
    assert!(sql[update].contains("CLOSED"));
    assert!(sql[audit].contains("SURVEY_AUTOMATICALLY_CLOSED"));
    assert!(!sql.iter().any(|s| s.contains("survey_response") || s.contains("survey_answer") || s.contains("survey_assignment")));
    assert_eq!(sql.last().unwrap(), "COMMIT");
}

#[tokio::test]
async fn opening_is_written_once_without_touching_frozen_assignments() {
    let fixture = Fixture::new();
    let db = fixture.db().await;
    assert_eq!(process_survey_with_clock(&db, fixture.tenant, fixture.survey, Utc::now).await.unwrap(), Some(ManagementAction::Opened));
    assert!(fixture.statements().iter().any(|s| s.contains("INSERT INTO \"audit_log\"")));
    let mut repeated = Fixture::new();
    repeated.opened = true;
    let db = repeated.db().await;
    assert_eq!(process_survey_with_clock(&db, repeated.tenant, repeated.survey, Utc::now).await.unwrap(), None);
    assert!(!repeated.statements().iter().any(|s| s.starts_with("INSERT") || s.starts_with("UPDATE")));
}

#[tokio::test]
async fn scheduler_does_not_repeat_a_manual_opening() {
    let mut fixture = Fixture::new();
    fixture.manually_opened = true;
    fixture.opens_at = Some(Utc::now() - Duration::seconds(1));
    let db = fixture.db().await;
    assert_eq!(process_survey_with_clock(&db, fixture.tenant, fixture.survey, Utc::now).await.unwrap(), None);
    assert!(!fixture.statements().iter().any(|s| s.starts_with("INSERT") || s.starts_with("UPDATE")));
}

#[tokio::test]
async fn failed_transition_rolls_back_and_deduplicated_failure_does_not_block_retry() {
    let mut fixture = Fixture::new();
    fixture.closes_at = Some(Utc::now() - Duration::seconds(1));
    fixture.fail_update = true;
    let db = fixture.db().await;
    assert!(process_survey_with_clock(&db, fixture.tenant, fixture.survey, Utc::now).await.is_err());
    assert!(fixture.statements().iter().any(|s| s == "ROLLBACK"));
    assert!(!fixture.statements().iter().any(|s| s.starts_with("INSERT")));
    record_transition_failure(&db, fixture.tenant, fixture.survey).await.unwrap();
    assert!(fixture.statements().iter().any(|s| s.contains("INSERT INTO \"audit_log\"") && s.contains("SURVEY_TRANSITION_FAILED")));
    fixture.failed = true;
    fixture.fail_update = false;
    fixture.sql = Arc::default();
    let db = fixture.db().await;
    record_transition_failure(&db, fixture.tenant, fixture.survey).await.unwrap();
    assert!(!fixture.statements().iter().any(|s| s.starts_with("INSERT")));
    assert_eq!(process_survey_with_clock(&db, fixture.tenant, fixture.survey, Utc::now).await.unwrap(), Some(ManagementAction::AutomaticallyClosed));
}
