//! Test the actual answer validation/persistence path without a live database.
use super::*;
use std::sync::{Arc, Mutex};
use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbErr, Iden, Iterable,
    ModelTrait, ProxyDatabaseTrait, ProxyExecResult, ProxyRow,
};

fn row<M: ModelTrait>(model: M) -> ProxyRow {
    ProxyRow::new(<M::Entity as EntityTrait>::Column::iter()
        .map(|column| (column.to_string(), model.get(column))).collect())
}

#[derive(Clone, Debug)]
struct Fixture {
    tenant: Uuid,
    id: Uuid,
    kind: &'static str,
    required: bool,
    self_rating: bool,
    manager_rating: bool,
    writes: Arc<Mutex<Vec<String>>>,
}

impl Fixture {
    fn new(kind: &'static str, required: bool) -> Self {
        Self { tenant: Uuid::new_v4(), id: Uuid::new_v4(), kind, required,
            self_rating: true, manager_rating: true, writes: Arc::default() }
    }

    fn participant(&self) -> performance_participant::Model {
        performance_participant::Model {
            id: self.id, tenant_id: self.tenant, review_cycle_id: self.id,
            employee_id: self.id, manager_employee_id: Some(self.id), department_id: None,
            designation_id: None, work_location_id: None, appraisal_template_id: self.id,
            status: "SELF_REVIEW".into(), is_excluded: false, exclusion_reason: None,
            response_revision: 1, self_submitted_at: None, manager_submitted_at: None,
            acknowledged_at: None, acknowledgement_comment: None, final_rating: None,
            performance_band: None, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(),
        }
    }

    fn answer(&self, text: Option<&str>, selected: bool, rating: Option<&str>) -> AppraisalAnswerInput {
        AppraisalAnswerInput {
            question_id: self.id.to_string().into(), text_answer: text.map(str::to_owned),
            selected_option_ids: if selected { vec![self.id.to_string().into()] } else { vec![] },
            rating: rating.map(str::to_owned),
        }
    }

    async fn save(&self, answers: Vec<AppraisalAnswerInput>, role: AnswerRole) -> Result<()> {
        let db: DatabaseConnection = Database::connect_proxy(
            DatabaseBackend::Postgres, Arc::new(Box::new(self.clone())),
        ).await.unwrap();
        save_appraisal_answers(&db, self.tenant, &self.participant(), answers, role).await
    }

    fn writes(&self) -> Vec<String> { self.writes.lock().unwrap().clone() }
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn query(&self, statement: Statement) -> std::result::Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        let now = chrono::Utc::now();
        if sql.contains("FROM \"appraisal_template_section\"") {
            return Ok(vec![row(appraisal_template_section::Model {
                id: self.id, tenant_id: self.tenant, appraisal_template_id: self.id,
                title: "Review".into(), description: None, display_order: 0, created_at: now,
            })]);
        }
        if sql.contains("FROM \"appraisal_question\"") {
            return Ok(vec![row(appraisal_question::Model {
                id: self.id, tenant_id: self.tenant, section_id: self.id, parent_question_id: None,
                question_type: self.kind.into(), prompt: "Progress".into(), is_required: self.required,
                answerer: "BOTH".into(), self_rating_enabled: self.self_rating,
                manager_rating_enabled: self.manager_rating, display_order: 0, created_at: now,
            })]);
        }
        if sql.contains("FROM \"appraisal_question_option\"") {
            return Ok(vec![row(appraisal_question_option::Model {
                id: self.id, tenant_id: self.tenant, question_id: self.id, label: "Done".into(),
                score: None, display_order: 0, created_at: now,
            })]);
        }
        if sql.contains("FROM performance_program p") {
            return Ok(vec![row(performance_program::Model {
                id: self.id, tenant_id: self.tenant, name: "Annual".into(), description: None,
                cadence: "MANUAL".into(), anchor_date: now.date_naive(), status: "ACTIVE".into(),
                include_calibration: false, include_acknowledgement: false,
                goal_weight_required: Decimal::ONE_HUNDRED, rating_min: Decimal::ONE,
                rating_max: Decimal::new(5, 0), created_by: None, created_at: now, updated_at: now,
            })]);
        }
        if sql.contains("FROM \"appraisal_answer\"") { return Ok(vec![]); }
        if sql.starts_with("INSERT INTO \"appraisal_answer\"") {
            self.writes.lock().unwrap().push(sql);
            return Ok(vec![row(appraisal_answer::Model {
                id: self.id, tenant_id: self.tenant, performance_participant_id: self.id,
                question_id: self.id, revision: 1, employee_text_answer: None,
                employee_selected_option_ids: None, self_rating: None, manager_text_answer: None,
                manager_selected_option_ids: None, manager_rating: None, created_at: now, updated_at: now,
            })]);
        }
        Err(DbErr::Custom(format!("unexpected validation query: {sql}")))
    }

    async fn execute(&self, statement: Statement) -> std::result::Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom(format!("unexpected execute: {statement}")))
    }
}

#[tokio::test]
async fn supplemental_rating_cannot_replace_required_text_or_choice() {
    for role in [AnswerRole::Employee, AnswerRole::Manager] {
        for kind in ["SHORT_TEXT", "LONG_TEXT", "SINGLE_CHOICE", "MULTIPLE_CHOICE"] {
            let fixture = Fixture::new(kind, true);
            let result = fixture.save(vec![fixture.answer(None, false, Some("3"))], role).await;
            assert!(result.is_err(), "{kind}: rating alone must not satisfy primary answer");
            assert!(result.unwrap_err().message.contains("Required question"));
            assert!(fixture.writes().is_empty(), "invalid answers must not persist");
        }
    }
}

#[tokio::test]
async fn comment_cannot_replace_required_rating() {
    for role in [AnswerRole::Employee, AnswerRole::Manager] {
        let fixture = Fixture::new("RATING", true);
        let result = fixture.save(vec![fixture.answer(Some("Explanation"), false, None)], role).await;
        assert!(result.is_err(), "rating question must require its rating");
        assert!(result.unwrap_err().message.contains("Required question"));
        assert!(fixture.writes().is_empty());
    }
}

#[tokio::test]
async fn whitespace_text_with_supplemental_rating_is_still_missing_primary() {
    let fixture = Fixture::new("LONG_TEXT", true);
    let result = fixture.save(vec![fixture.answer(Some(" \t "), false, Some("3"))], AnswerRole::Employee).await;
    assert!(result.is_err());
    assert!(result.unwrap_err().message.contains("Required question"));
    assert!(fixture.writes().is_empty());
}

#[tokio::test]
async fn valid_primary_answers_persist_for_both_reviewers_without_extra_rating_requirement() {
    for role in [AnswerRole::Employee, AnswerRole::Manager] {
        for (kind, text, selected, rating) in [
            ("SHORT_TEXT", Some("Done"), false, None),
            ("LONG_TEXT", Some("Details"), false, None),
            ("SINGLE_CHOICE", None, true, None),
            ("MULTIPLE_CHOICE", None, true, None),
            ("RATING", Some("Optional comment"), false, Some("3")),
        ] {
            let fixture = Fixture::new(kind, true);
            let result = fixture.save(vec![fixture.answer(text, selected, rating)], role).await;
            assert!(result.is_ok(), "{kind}: {result:?}");
            assert_eq!(fixture.writes().len(), 1);
        }
    }
}

#[tokio::test]
async fn optional_questions_can_be_omitted_or_left_blank() {
    for kind in ["SHORT_TEXT", "LONG_TEXT", "SINGLE_CHOICE", "MULTIPLE_CHOICE", "RATING"] {
        let fixture = Fixture::new(kind, false);
        assert!(fixture.save(vec![], AnswerRole::Employee).await.is_ok());
        assert!(fixture.writes().is_empty());
        assert!(fixture.save(vec![fixture.answer(None, false, None)], AnswerRole::Employee).await.is_ok());
        assert_eq!(fixture.writes().len(), 1);
    }
}

#[tokio::test]
async fn omitted_required_question_and_out_of_range_rating_stay_rejected() {
    let fixture = Fixture::new("RATING", true);
    assert!(fixture.save(vec![], AnswerRole::Manager).await.is_err());
    assert!(fixture.save(vec![fixture.answer(None, false, Some("6"))], AnswerRole::Manager).await.is_err());
    assert!(fixture.writes().is_empty());
}

#[tokio::test]
async fn supplemental_rating_permission_is_specific_to_reviewer() {
    for disabled_role in [AnswerRole::Employee, AnswerRole::Manager] {
        let mut fixture = Fixture::new("LONG_TEXT", true);
        fixture.self_rating = disabled_role != AnswerRole::Employee;
        fixture.manager_rating = disabled_role != AnswerRole::Manager;
        let enabled_role = if disabled_role == AnswerRole::Employee {
            AnswerRole::Manager
        } else {
            AnswerRole::Employee
        };
        let answer = fixture.answer(Some("Done"), false, Some("3"));
        let error = fixture.save(vec![answer.clone()], disabled_role).await.unwrap_err();
        assert!(error.message.contains("Rating is not enabled"));
        assert!(fixture.writes().is_empty());
        assert!(fixture.save(vec![answer], enabled_role).await.is_ok());
        assert_eq!(fixture.writes().len(), 1);
    }
}
