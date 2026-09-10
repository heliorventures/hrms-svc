//! Transactional, privacy-preserving survey submission persistence.

use std::collections::{HashMap, HashSet};

use chrono::{DateTime, Utc};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee,
    d0076_anonymous_surveys::{
        survey, survey_answer, survey_assignment, survey_question, survey_question_option,
        survey_response, survey_section,
    },
};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, DatabaseConnection, EntityTrait, IntoActiveModel, QueryFilter,
    QuerySelect, Set, TransactionTrait,
};
use uuid::Uuid;

/// A parsed answer supplied by a survey respondent.
#[derive(Clone, Debug)]
pub struct SubmissionAnswer {
    pub question_id: Uuid,
    pub selected_option_ids: Vec<Uuid>,
    pub numeric_answer: Option<Decimal>,
    pub text_answer: Option<String>,
    pub comment: Option<String>,
}

/// Builds an assignment with the cohort values visible at publication time.
pub fn new_assignment(
    tenant_id: Uuid,
    survey_id: Uuid,
    employee: &employee::Model,
    published_at: DateTime<Utc>,
) -> survey_assignment::ActiveModel {
    survey_assignment::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        survey_id: Set(survey_id),
        employee_id: Set(employee.id),
        created_at: Set(published_at),
        completed: Set(false),
        publication_department_id: Set(employee.department_id),
        publication_manager_employee_id: Set(employee.reporting_manager_id),
        publication_location_id: Set(employee.location_id),
    }
}

/// Validates and persists a response using only the assignment's frozen cohort.
pub async fn submit_survey(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    survey_id: Uuid,
    employee_id: Uuid,
    answers: Vec<SubmissionAnswer>,
) -> KabiPayResult<bool> {
    submit_survey_with_clock(db, tenant_id, survey_id, employee_id, answers, Utc::now).await
}

pub(super) async fn submit_survey_with_clock<F>(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    survey_id: Uuid,
    employee_id: Uuid,
    answers: Vec<SubmissionAnswer>,
    clock: F,
) -> KabiPayResult<bool>
where
    F: FnOnce() -> DateTime<Utc>,
{
    let txn = db.begin().await?;
    let survey = survey::Entity::find_by_id(survey_id)
        .filter(survey::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "survey",
            id: survey_id.to_string(),
        })?;
    let now = clock();
    if survey.status != "PUBLISHED"
        || survey.opens_at.is_some_and(|opens| now < opens)
        || survey.closes_at.is_some_and(|closes| now >= closes)
    {
        return Err(KabiPayError::Validation(
            "This survey is not currently open for responses".into(),
        ));
    }

    let assignment = survey_assignment::Entity::find()
        .filter(survey_assignment::Column::TenantId.eq(tenant_id))
        .filter(survey_assignment::Column::SurveyId.eq(survey_id))
        .filter(survey_assignment::Column::EmployeeId.eq(employee_id))
        .lock_exclusive()
        .one(&txn)
        .await?
        .ok_or_else(|| {
            KabiPayError::Forbidden("This survey is not assigned to the signed-in employee".into())
        })?;
    if assignment.completed {
        return Err(KabiPayError::Conflict(
            "This survey has already been submitted".into(),
        ));
    }

    let sections = survey_section::Entity::find()
        .filter(survey_section::Column::TenantId.eq(tenant_id))
        .filter(survey_section::Column::SurveyId.eq(survey_id))
        .all(&txn)
        .await?;
    let section_ids: Vec<Uuid> = sections.into_iter().map(|row| row.id).collect();
    let questions = survey_question::Entity::find()
        .filter(survey_question::Column::TenantId.eq(tenant_id))
        .filter(survey_question::Column::SectionId.is_in(section_ids))
        .all(&txn)
        .await?;
    let question_map: HashMap<Uuid, survey_question::Model> =
        questions.into_iter().map(|row| (row.id, row)).collect();
    let answers = normalize_answers(answers);
    validate_answers(&txn, tenant_id, &question_map, &answers).await?;

    let response_id = Uuid::new_v4();
    survey_response::ActiveModel {
        id: Set(response_id),
        tenant_id: Set(tenant_id),
        survey_id: Set(survey_id),
        department_id: Set(assignment.publication_department_id),
        manager_employee_id: Set(assignment.publication_manager_employee_id),
        location_id: Set(assignment.publication_location_id),
    }
    .insert(&txn)
    .await?;
    for answer in answers {
        survey_answer::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            survey_response_id: Set(response_id),
            question_id: Set(answer.question_id),
            selected_option_ids: Set((!answer.selected_option_ids.is_empty()).then(|| {
                serde_json::Value::Array(
                    answer
                        .selected_option_ids
                        .into_iter()
                        .map(|id| serde_json::Value::String(id.to_string()))
                        .collect(),
                )
            })),
            numeric_answer: Set(answer.numeric_answer),
            text_answer: Set(answer.text_answer),
            comment: Set(answer.comment),
        }
        .insert(&txn)
        .await?;
    }
    let mut assignment = assignment.into_active_model();
    assignment.completed = Set(true);
    assignment.update(&txn).await?;
    txn.commit().await?;
    Ok(true)
}

fn normalize_answers(answers: Vec<SubmissionAnswer>) -> Vec<SubmissionAnswer> {
    answers
        .into_iter()
        .map(|mut answer| {
            answer.comment = answer.comment.as_deref().map(str::trim).filter(|text| !text.is_empty()).map(str::to_owned);
            answer.text_answer = answer
                .text_answer
                .as_deref()
                .map(str::trim)
                .filter(|text| !text.is_empty())
                .map(str::to_owned);
            answer
        })
        .collect()
}

async fn validate_answers(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    question_map: &HashMap<Uuid, survey_question::Model>,
    answers: &[SubmissionAnswer],
) -> KabiPayResult<()> {
    let mut answered = HashSet::new();
    for answer in answers {
        if !answered.insert(answer.question_id) {
            return Err(KabiPayError::Validation(
                "Each survey question can be answered only once".into(),
            ));
        }
        let question = question_map.get(&answer.question_id).ok_or_else(|| {
            KabiPayError::Validation("An answer references a question outside this survey".into())
        })?;
        if answer.comment.as_ref().is_some_and(|comment| !question.comment_enabled || comment.chars().count() > 4000) {
            return Err(KabiPayError::Validation("Additional comment is disabled or exceeds 4000 characters".into()));
        }
        let text_answer = answer.text_answer.as_deref().map(str::trim).filter(|text| !text.is_empty());
        if text_answer.is_some_and(|text| text.chars().count() > 8_000) {
            return Err(KabiPayError::Validation(
                "Text answer must contain 1 to 8000 characters".into(),
            ));
        }
        let value_count = usize::from(!answer.selected_option_ids.is_empty())
            + usize::from(answer.numeric_answer.is_some())
            + usize::from(text_answer.is_some());
        if value_count != 1 {
            return Err(KabiPayError::Validation(
                "Each answered survey question must provide exactly one compatible value".into(),
            ));
        }
        match question.question_type.as_str() {
            "SINGLE_CHOICE" if answer.selected_option_ids.len() != 1 => {
                return Err(KabiPayError::Validation(
                    "Single-choice questions require exactly one option".into(),
                ));
            }
            "SINGLE_CHOICE" | "MULTIPLE_CHOICE"
                if answer.numeric_answer.is_some() || text_answer.is_some() =>
            {
                return Err(KabiPayError::Validation(
                    "Choice questions require selected options".into(),
                ));
            }
            "RATING" if answer.numeric_answer.is_none() => {
                return Err(KabiPayError::Validation(
                    "Rating questions require a numeric answer".into(),
                ));
            }
            "SHORT_TEXT" | "LONG_TEXT" if text_answer.is_none() => {
                return Err(KabiPayError::Validation(
                    "Text questions require a text answer".into(),
                ));
            }
            _ => {}
        }
        if let Some(value) = answer.numeric_answer {
            if value < question.rating_min.unwrap_or(value) || value > question.rating_max.unwrap_or(value) {
                return Err(KabiPayError::Validation(
                    "A rating answer is outside the configured range".into(),
                ));
            }
        }
    }
    for question in question_map.values().filter(|question| question.is_required) {
        if !answered.contains(&question.id) {
            return Err(KabiPayError::Validation(format!(
                "Required question '{}' must be answered",
                question.prompt
            )));
        }
    }
    let all_options: Vec<Uuid> = answers
        .iter()
        .flat_map(|answer| answer.selected_option_ids.iter().copied())
        .collect();
    if all_options.is_empty() {
        return Ok(());
    }
    let valid = survey_question_option::Entity::find()
        .filter(survey_question_option::Column::TenantId.eq(tenant_id))
        .filter(survey_question_option::Column::Id.is_in(all_options.clone()))
        .all(txn)
        .await?;
    if valid.len() != all_options.len()
        || valid.iter().any(|option| {
            !answers.iter().any(|answer| {
                answer.question_id == option.question_id
                    && answer.selected_option_ids.contains(&option.id)
            })
        })
    {
        return Err(KabiPayError::Validation(
            "A selected option does not belong to its survey question".into(),
        ));
    }
    Ok(())
}
