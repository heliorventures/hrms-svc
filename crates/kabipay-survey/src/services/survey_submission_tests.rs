use std::{
    collections::BTreeMap,
    sync::{Arc, Mutex},
};

use chrono::{DateTime, Utc};
use kabipay_common::KabiPayError;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use rust_decimal::Decimal;
use sea_orm::{
    entity::prelude::async_trait, ActiveValue, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use uuid::Uuid;

use super::{
    survey_service::{completion_for_viewer, load_optional_assignment},
    survey_submission::{new_assignment, submit_survey, submit_survey_with_clock, SubmissionAnswer},
};

#[derive(Clone, Debug)]
struct SurveyFixture {
    tenant_id: Uuid,
    survey_id: Uuid,
    employee_id: Uuid,
    option_id: Uuid,
    department_id: Option<Uuid>,
    manager_id: Option<Uuid>,
    question_type: String,
    closes_at: Option<DateTime<Utc>>,
    completed: bool,
    fail_assignment_lookup: bool,
    fail_answer_insert: bool,
    events: Arc<Mutex<Vec<String>>>,
}

impl SurveyFixture {
    fn new() -> Self {
        Self {
            tenant_id: Uuid::new_v4(),
            survey_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(),
            option_id: Uuid::new_v4(),
            department_id: Some(Uuid::new_v4()),
            manager_id: Some(Uuid::new_v4()),
            question_type: "RATING".into(),
            closes_at: None,
            completed: false,
            fail_assignment_lookup: false,
            fail_answer_insert: false,
            events: Arc::new(Mutex::new(Vec::new())),
        }
    }

    async fn connection(&self) -> DatabaseConnection {
        Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(self.clone())))
            .await
            .expect("PostgreSQL proxy connection")
    }

    fn event(&self, event: impl Into<String>) {
        self.events.lock().expect("survey event recorder").push(event.into());
    }

    fn events(&self) -> Vec<String> {
        self.events.lock().expect("survey event recorder").clone()
    }

    fn survey_row(&self) -> ProxyRow {
        let now = Utc::now();
        ProxyRow::new(BTreeMap::from([
            ("id".into(), self.survey_id.into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("title".into(), "Pulse".into()),
            ("description".into(), Option::<String>::None.into()),
            ("status".into(), "PUBLISHED".into()),
            ("opens_at".into(), Option::<DateTime<Utc>>::None.into()),
            ("closes_at".into(), self.closes_at.into()),
            ("minimum_report_group_size".into(), 3i32.into()),
            ("created_by".into(), Uuid::new_v4().into()),
            ("published_at".into(), Some(now).into()),
            ("closed_at".into(), Option::<DateTime<Utc>>::None.into()),
            ("created_at".into(), now.into()),
            ("updated_at".into(), now.into()),
        ]))
    }

    fn assignment_row(&self, completed: bool) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::new_v4().into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("survey_id".into(), self.survey_id.into()),
            ("employee_id".into(), self.employee_id.into()),
            ("completed".into(), completed.into()),
            ("publication_department_id".into(), self.department_id.into()),
            ("publication_manager_employee_id".into(), self.manager_id.into()),
            ("created_at".into(), Utc::now().into()),
        ]))
    }

    fn section_row(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::nil().into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("survey_id".into(), self.survey_id.into()),
            ("title".into(), "General".into()),
            ("display_order".into(), 0i32.into()),
        ]))
    }

    fn question_row(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::nil().into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("section_id".into(), Uuid::nil().into()),
            ("dimension".into(), "Wellbeing".into()),
            ("question_type".into(), self.question_type.clone().into()),
            ("prompt".into(), "Score".into()),
            ("is_required".into(), true.into()),
            ("rating_min".into(), Some(Decimal::ONE).into()),
            ("rating_max".into(), Some(Decimal::new(5, 0)).into()),
            ("display_order".into(), 0i32.into()),
        ]))
    }

    fn response_row(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::new_v4().into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("survey_id".into(), self.survey_id.into()),
            ("department_id".into(), self.department_id.into()),
            ("manager_employee_id".into(), self.manager_id.into()),
        ]))
    }

    fn option_row(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), self.option_id.into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("question_id".into(), Uuid::nil().into()),
            ("label".into(), "Choice".into()),
            ("score".into(), Option::<Decimal>::None.into()),
            ("display_order".into(), 0i32.into()),
        ]))
    }

    fn answer_row(&self) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::new_v4().into()),
            ("tenant_id".into(), self.tenant_id.into()),
            ("survey_response_id".into(), Uuid::new_v4().into()),
            ("question_id".into(), Uuid::nil().into()),
            ("selected_option_ids".into(), Option::<serde_json::Value>::None.into()),
            ("numeric_answer".into(), Some(Decimal::new(4, 0)).into()),
            ("text_answer".into(), Option::<String>::None.into()),
        ]))
    }
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for SurveyFixture {
    async fn begin(&self) {
        self.event("BEGIN");
    }

    async fn commit(&self) {
        self.event("COMMIT");
    }

    async fn rollback(&self) {
        self.event("ROLLBACK");
    }

    fn start_rollback(&self) {
        self.event("ROLLBACK");
    }

    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.event(sql.clone());
        if sql.contains("FROM \"survey_assignment\"") {
            if self.fail_assignment_lookup {
                return Err(DbErr::Custom("simulated assignment lookup failure".into()));
            }
            return Ok(vec![self.assignment_row(self.completed)]);
        }
        if sql.contains("FROM \"survey_section\"") {
            return Ok(vec![self.section_row()]);
        }
        if sql.contains("FROM \"survey_question\"") {
            return Ok(vec![self.question_row()]);
        }
        if sql.contains("FROM \"survey_question_option\"") {
            return Ok(vec![self.option_row()]);
        }
        if sql.contains("FROM \"survey\"") {
            return Ok(vec![self.survey_row()]);
        }
        if sql.starts_with("INSERT INTO \"survey_response\"") {
            return Ok(vec![self.response_row()]);
        }
        if sql.starts_with("INSERT INTO \"survey_answer\"") {
            if self.fail_answer_insert {
                return Err(DbErr::Custom("simulated answer constraint failure".into()));
            }
            return Ok(vec![self.answer_row()]);
        }
        if sql.starts_with("UPDATE \"survey_assignment\"") {
            return Ok(vec![self.assignment_row(true)]);
        }
        if sql.contains("FROM \"employee\"") {
            return Err(DbErr::Custom(
                "submission must not read mutable employee membership".into(),
            ));
        }
        Err(DbErr::Custom(format!("unexpected survey query: {sql}")))
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.event(statement.to_string());
        Ok(ProxyExecResult {
            last_insert_id: 0,
            rows_affected: 1,
        })
    }
}

fn employee_model(
    tenant_id: Uuid,
    employee_id: Uuid,
    department_id: Option<Uuid>,
    manager_id: Option<Uuid>,
) -> employee::Model {
    let now = Utc::now();
    employee::Model {
        id: employee_id,
        tenant_id,
        user_id: Some(Uuid::new_v4()),
        department_id,
        designation_id: None,
        cost_center_id: None,
        location_id: None,
        reporting_manager_id: manager_id,
        employee_code: "EMP-1".into(),
        first_name: "Test".into(),
        last_name: "Employee".into(),
        date_of_birth: None,
        gender: None,
        blood_group: None,
        nationality: None,
        employment_type: Some("FULL_TIME".into()),
        status: "ACTIVE".into(),
        date_of_joining: chrono::NaiveDate::from_ymd_opt(2026, 1, 1).expect("valid date"),
        probation_end_date: None,
        notice_period_days: None,
        emergency_contact_name: None,
        emergency_contact_phone: None,
        emergency_contact_relation: None,
        personal_phone: None,
        current_address: None,
        permanent_address: None,
        uan_number: None,
        esic_number: None,
        is_deleted: false,
        deleted_at: None,
        deleted_by: None,
        created_at: now,
        updated_at: now,
    }
}

fn rating_answer() -> SubmissionAnswer {
    SubmissionAnswer {
        question_id: Uuid::nil(),
        selected_option_ids: Vec::new(),
        numeric_answer: Some(Decimal::new(4, 0)),
        text_answer: None,
    }
}

fn answer_insert(events: &[String]) -> &str {
    events
        .iter()
        .find(|event| event.starts_with("INSERT INTO \"survey_answer\""))
        .expect("survey answer insert")
}

#[test]
fn publication_assignment_freezes_department_and_manager_with_nulls_preserved() {
    let tenant_id = Uuid::new_v4();
    let survey_id = Uuid::new_v4();
    let employee_id = Uuid::new_v4();
    let department_at_publication = Uuid::new_v4();
    let manager_at_publication = Uuid::new_v4();
    let mut employee = employee_model(
        tenant_id,
        employee_id,
        Some(department_at_publication),
        Some(manager_at_publication),
    );

    let assignment = new_assignment(tenant_id, survey_id, &employee, Utc::now());
    employee.department_id = Some(Uuid::new_v4());
    employee.reporting_manager_id = Some(Uuid::new_v4());

    assert_eq!(assignment.completed, ActiveValue::Set(false));
    assert_eq!(
        assignment.publication_department_id,
        ActiveValue::Set(Some(department_at_publication))
    );
    assert_eq!(
        assignment.publication_manager_employee_id,
        ActiveValue::Set(Some(manager_at_publication))
    );

    let null_employee = employee_model(tenant_id, employee_id, None, None);
    let null_assignment = new_assignment(tenant_id, survey_id, &null_employee, Utc::now());
    assert_eq!(null_assignment.publication_department_id, ActiveValue::Set(None));
    assert_eq!(
        null_assignment.publication_manager_employee_id,
        ActiveValue::Set(None)
    );
}

#[tokio::test]
async fn submission_uses_publication_snapshots_and_completes_after_answers() {
    let fixture = SurveyFixture::new();
    let db = fixture.connection().await;

    let submitted = submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![rating_answer()],
    )
    .await
    .expect("valid survey submission");

    assert!(submitted);
    let events = fixture.events();
    let response = events
        .iter()
        .position(|event| event.starts_with("INSERT INTO \"survey_response\""))
        .expect("anonymous response insert");
    let answer = events
        .iter()
        .position(|event| event.starts_with("INSERT INTO \"survey_answer\""))
        .expect("answer insert");
    let completion = events
        .iter()
        .position(|event| event.starts_with("UPDATE \"survey_assignment\""))
        .expect("assignment completion update");
    let commit = events.iter().position(|event| event == "COMMIT").expect("commit");
    assert!(response < answer && answer < completion && completion < commit, "{events:#?}");
    assert!(events[response].contains(&fixture.department_id.expect("department").to_string()));
    assert!(events[response].contains(&fixture.manager_id.expect("manager").to_string()));
    assert!(events.iter().all(|event| !event.contains("FROM \"employee\"")));
    for select in events.iter().filter(|event| event.starts_with("SELECT")) {
        assert!(select.contains(&fixture.tenant_id.to_string()), "{select}");
    }
}

#[tokio::test]
async fn null_publication_snapshots_do_not_fall_back_to_current_employee() {
    let mut fixture = SurveyFixture::new();
    fixture.department_id = None;
    fixture.manager_id = None;
    let db = fixture.connection().await;

    submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![rating_answer()],
    )
    .await
    .expect("valid survey submission");

    let response = fixture
        .events()
        .into_iter()
        .find(|event| event.starts_with("INSERT INTO \"survey_response\""))
        .expect("anonymous response insert");
    assert!(response.contains("NULL"), "{response}");
    assert!(!response.contains("submitted_at"), "{response}");
}

#[tokio::test]
async fn numeric_answer_normalizes_whitespace_text_to_null_before_persistence() {
    let fixture = SurveyFixture::new();
    let db = fixture.connection().await;
    let mut answer = rating_answer();
    answer.text_answer = Some("   ".into());

    submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![answer],
    )
    .await
    .expect("numeric answer with whitespace-only text");

    let events = fixture.events();
    let insert = answer_insert(&events);
    assert!(insert.contains("NULL) RETURNING"), "{insert}");
    assert!(!insert.contains("'   '"), "{insert}");
}

#[tokio::test]
async fn choice_answer_normalizes_whitespace_text_to_null_before_persistence() {
    let mut fixture = SurveyFixture::new();
    fixture.question_type = "SINGLE_CHOICE".into();
    let db = fixture.connection().await;

    submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![SubmissionAnswer {
            question_id: Uuid::nil(),
            selected_option_ids: vec![fixture.option_id],
            numeric_answer: None,
            text_answer: Some("   ".into()),
        }],
    )
    .await
    .expect("choice answer with whitespace-only text");

    let events = fixture.events();
    let insert = answer_insert(&events);
    assert!(insert.contains("NULL) RETURNING"), "{insert}");
    assert!(!insert.contains("'   '"), "{insert}");
}

#[tokio::test]
async fn text_answer_persists_trimmed_validated_text() {
    let mut fixture = SurveyFixture::new();
    fixture.question_type = "SHORT_TEXT".into();
    let db = fixture.connection().await;

    submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![SubmissionAnswer {
            question_id: Uuid::nil(),
            selected_option_ids: Vec::new(),
            numeric_answer: None,
            text_answer: Some("  clear feedback  ".into()),
        }],
    )
    .await
    .expect("trimmed text answer");

    let events = fixture.events();
    let insert = answer_insert(&events);
    assert!(insert.contains("clear feedback"), "{insert}");
    assert!(!insert.contains("  clear feedback  "), "{insert}");
}

#[tokio::test]
async fn submission_uses_clock_sampled_after_survey_lock() {
    let mut fixture = SurveyFixture::new();
    let closes_at = Utc::now();
    fixture.closes_at = Some(closes_at);
    let db = fixture.connection().await;

    let error = submit_survey_with_clock(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![rating_answer()],
        || closes_at + chrono::Duration::seconds(1),
    )
    .await
    .expect_err("a post-lock clock after close must reject the response");

    assert!(matches!(error, KabiPayError::Validation(_)));
    assert!(
        fixture
            .events()
            .iter()
            .all(|event| !event.starts_with("INSERT INTO \"survey_response\""))
    );
}

#[tokio::test]
async fn answer_failure_rolls_back_without_marking_assignment_complete() {
    let mut fixture = SurveyFixture::new();
    fixture.fail_answer_insert = true;
    let db = fixture.connection().await;

    let error = submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![rating_answer()],
    )
    .await
    .expect_err("answer insertion failure must abort submission");

    assert!(matches!(error, KabiPayError::Database(_)));
    let events = fixture.events();
    assert!(events.iter().any(|event| event == "ROLLBACK"), "{events:#?}");
    assert!(
        events
            .iter()
            .all(|event| !event.starts_with("UPDATE \"survey_assignment\"")),
        "{events:#?}"
    );
    assert!(events.iter().all(|event| event != "COMMIT"), "{events:#?}");
}

#[tokio::test]
async fn completed_assignment_conflicts_before_response_creation() {
    let mut fixture = SurveyFixture::new();
    fixture.completed = true;
    let db = fixture.connection().await;

    let error = submit_survey(
        &db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
        vec![rating_answer()],
    )
    .await
    .expect_err("second submission must conflict");

    assert!(matches!(error, KabiPayError::Conflict(_)));
    assert!(
        fixture
            .events()
            .iter()
            .all(|event| !event.starts_with("INSERT INTO \"survey_response\""))
    );
}

#[tokio::test]
async fn administrator_completion_variants_and_ordinary_assignment_access_are_distinct() {
    let mut assigned = SurveyFixture::new();
    assigned.completed = true;
    let assigned_db = assigned.connection().await;
    assert!(
        completion_for_viewer(
            &assigned_db,
            assigned.tenant_id,
            assigned.survey_id,
            Some(assigned.employee_id),
            true,
        )
        .await
        .expect("assigned administrator completion")
    );

    let unlinked = SurveyFixture::new();
    let unlinked_db = unlinked.connection().await;
    assert!(
        !completion_for_viewer(
            &unlinked_db,
            unlinked.tenant_id,
            unlinked.survey_id,
            None,
            true,
        )
        .await
        .expect("unlinked administrator access")
    );
    assert!(unlinked.events().is_empty());

    let unassigned = SurveyFixture::new();
    let unassigned_db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(MissingAssignmentProxy {
            events: Arc::clone(&unassigned.events),
        })),
    )
    .await
    .expect("missing-assignment proxy");
    assert!(
        !completion_for_viewer(
            &unassigned_db,
            unassigned.tenant_id,
            unassigned.survey_id,
            Some(unassigned.employee_id),
            true,
        )
        .await
        .expect("unassigned administrator access")
    );
    let ordinary = completion_for_viewer(
        &unassigned_db,
        unassigned.tenant_id,
        unassigned.survey_id,
        Some(unassigned.employee_id),
        false,
    )
    .await
    .expect_err("ordinary unassigned respondents must be forbidden");
    assert!(matches!(ordinary, KabiPayError::Forbidden(_)));
}

#[derive(Clone, Debug)]
struct MissingAssignmentProxy {
    events: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for MissingAssignmentProxy {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.events
            .lock()
            .expect("missing-assignment recorder")
            .push(statement.to_string());
        Ok(Vec::new())
    }

    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        panic!("assignment lookup must not write")
    }
}

#[tokio::test]
async fn optional_assignment_preserves_missing_and_database_failure() {
    let fixture = SurveyFixture::new();
    let missing_db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(MissingAssignmentProxy {
            events: Arc::clone(&fixture.events),
        })),
    )
    .await
    .expect("missing-assignment proxy");
    let missing = load_optional_assignment(
        &missing_db,
        fixture.tenant_id,
        fixture.survey_id,
        fixture.employee_id,
    )
    .await
    .expect("missing assignment is not a database error");
    assert!(missing.is_none());
    let sql = fixture.events()[0].clone();
    assert!(sql.contains(&fixture.tenant_id.to_string()), "{sql}");
    assert!(sql.contains(&fixture.survey_id.to_string()), "{sql}");
    assert!(sql.contains(&fixture.employee_id.to_string()), "{sql}");

    let mut failing = SurveyFixture::new();
    failing.fail_assignment_lookup = true;
    let failing_db = failing.connection().await;
    let error = load_optional_assignment(
        &failing_db,
        failing.tenant_id,
        failing.survey_id,
        failing.employee_id,
    )
    .await
    .expect_err("database failure must not look like missing assignment");
    assert!(matches!(error, KabiPayError::Database(_)));
}
