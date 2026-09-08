//! Tenant-scoped survey persistence and aggregate reporting.

use std::collections::HashMap;

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0076_anonymous_surveys::{
    survey, survey_answer, survey_assignment, survey_audience_department, survey_question,
    survey_question_option, survey_response, survey_section,
};
use rust_decimal::Decimal;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder};
use uuid::Uuid;

use crate::resolvers::types::{
    SurveyDimensionAggregateDto, SurveyDto, SurveyOptionAggregateDto,
    SurveyQuestionAggregateDto, SurveyQuestionDto, SurveyResultsDto, SurveySectionDto,
    SurveySummaryDto,
};
use crate::services::survey_rules::report_group_is_visible;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ResultScope {
    All,
    Department(Uuid),
    Team(Uuid),
}

pub async fn list_surveys(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<SurveySummaryDto>> {
    survey::Entity::find()
        .filter(survey::Column::TenantId.eq(tenant_id))
        .order_by_desc(survey::Column::CreatedAt)
        .all(db)
        .await
        .map(|rows| rows.into_iter().map(|row| SurveySummaryDto::from_model(row, false)).collect())
        .map_err(KabiPayError::from)
}

pub async fn list_results_surveys(
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> KabiPayResult<Vec<SurveySummaryDto>> {
    survey::Entity::find()
        .filter(survey::Column::TenantId.eq(tenant_id))
        .filter(survey::Column::Status.ne("DRAFT"))
        .order_by_desc(survey::Column::CreatedAt)
        .all(db)
        .await
        .map(|rows| rows.into_iter().map(|row| SurveySummaryDto::from_model(row, false)).collect())
        .map_err(KabiPayError::from)
}

pub async fn list_available_surveys(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<Vec<SurveySummaryDto>> {
    let assignments = survey_assignment::Entity::find()
        .filter(survey_assignment::Column::TenantId.eq(tenant_id))
        .filter(survey_assignment::Column::EmployeeId.eq(employee_id))
        .order_by_desc(survey_assignment::Column::CreatedAt)
        .all(db)
        .await?;
    let survey_ids: Vec<Uuid> = assignments.iter().map(|row| row.survey_id).collect();
    if survey_ids.is_empty() {
        return Ok(Vec::new());
    }
    let surveys = survey::Entity::find()
        .filter(survey::Column::TenantId.eq(tenant_id))
        .filter(survey::Column::Id.is_in(survey_ids))
        .order_by_desc(survey::Column::CreatedAt)
        .all(db)
        .await?;
    let completion: HashMap<Uuid, bool> = assignments
        .into_iter()
        .map(|row| (row.survey_id, row.completed_at.is_some()))
        .collect();
    Ok(surveys
        .into_iter()
        .map(|row| {
            let completed = completion.get(&row.id).copied().unwrap_or(false);
            SurveySummaryDto::from_model(row, completed)
        })
        .collect())
}

pub async fn load_survey_model(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    survey_id: Uuid,
) -> KabiPayResult<survey::Model> {
    survey::Entity::find_by_id(survey_id)
        .filter(survey::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: survey_id.to_string() })
}

pub async fn load_survey(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    survey_id: Uuid,
    completed: bool,
) -> KabiPayResult<SurveyDto> {
    let survey = load_survey_model(db, tenant_id, survey_id).await?;
    let audience = survey_audience_department::Entity::find()
        .filter(survey_audience_department::Column::TenantId.eq(tenant_id))
        .filter(survey_audience_department::Column::SurveyId.eq(survey_id))
        .all(db)
        .await?;
    let sections = survey_section::Entity::find()
        .filter(survey_section::Column::TenantId.eq(tenant_id))
        .filter(survey_section::Column::SurveyId.eq(survey_id))
        .order_by_asc(survey_section::Column::DisplayOrder)
        .all(db)
        .await?;
    let section_ids: Vec<Uuid> = sections.iter().map(|section| section.id).collect();
    let questions = if section_ids.is_empty() { Vec::new() } else {
        survey_question::Entity::find()
            .filter(survey_question::Column::TenantId.eq(tenant_id))
            .filter(survey_question::Column::SectionId.is_in(section_ids))
            .order_by_asc(survey_question::Column::DisplayOrder)
            .all(db)
            .await?
    };
    let question_ids: Vec<Uuid> = questions.iter().map(|question| question.id).collect();
    let options = if question_ids.is_empty() { Vec::new() } else {
        survey_question_option::Entity::find()
            .filter(survey_question_option::Column::TenantId.eq(tenant_id))
            .filter(survey_question_option::Column::QuestionId.is_in(question_ids))
            .order_by_asc(survey_question_option::Column::DisplayOrder)
            .all(db)
            .await?
    };
    let sections = sections.into_iter().map(|section| {
        let questions = questions.iter().filter(|question| question.section_id == section.id)
            .cloned().map(|question| {
                let question_options = options.iter().filter(|option| option.question_id == question.id).cloned().collect();
                SurveyQuestionDto::from_model(question, question_options)
            }).collect();
        SurveySectionDto::from_model(section, questions)
    }).collect();
    Ok(SurveyDto {
        summary: SurveySummaryDto::from_model(survey, completed),
        audience_department_ids: audience.into_iter().map(|row| row.department_id.to_string().into()).collect(),
        sections,
    })
}

pub async fn load_assignment(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    survey_id: Uuid,
    employee_id: Uuid,
) -> KabiPayResult<survey_assignment::Model> {
    survey_assignment::Entity::find()
        .filter(survey_assignment::Column::TenantId.eq(tenant_id))
        .filter(survey_assignment::Column::SurveyId.eq(survey_id))
        .filter(survey_assignment::Column::EmployeeId.eq(employee_id))
        .one(db).await?
        .ok_or_else(|| KabiPayError::Forbidden("This survey is not assigned to the signed-in employee".into()))
}

fn selected_ids(value: &Option<serde_json::Value>) -> Vec<Uuid> {
    value.as_ref().and_then(serde_json::Value::as_array).into_iter().flatten()
        .filter_map(|value| value.as_str()).filter_map(|value| Uuid::parse_str(value).ok()).collect()
}

pub async fn aggregate_results(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    survey_id: Uuid,
    scope: ResultScope,
) -> KabiPayResult<SurveyResultsDto> {
    let survey = load_survey_model(db, tenant_id, survey_id).await?;
    let mut query = survey_response::Entity::find()
        .filter(survey_response::Column::TenantId.eq(tenant_id))
        .filter(survey_response::Column::SurveyId.eq(survey_id));
    query = match scope {
        ResultScope::All => query,
        ResultScope::Department(department_id) => query.filter(survey_response::Column::DepartmentId.eq(department_id)),
        ResultScope::Team(manager_id) => query.filter(survey_response::Column::ManagerEmployeeId.eq(manager_id)),
    };
    let responses = query.all(db).await?;
    let threshold = survey.minimum_report_group_size.max(3) as usize;
    if !report_group_is_visible(responses.len(), threshold) {
        return Ok(SurveyResultsDto {
            survey_id: survey_id.to_string().into(), suppressed: true, respondent_count: None,
            minimum_report_group_size: threshold as i32, dimensions: Vec::new(), questions: Vec::new(),
        });
    }
    let response_ids: Vec<Uuid> = responses.iter().map(|response| response.id).collect();
    let sections = survey_section::Entity::find()
        .filter(survey_section::Column::TenantId.eq(tenant_id))
        .filter(survey_section::Column::SurveyId.eq(survey_id)).all(db).await?;
    let section_ids: Vec<Uuid> = sections.iter().map(|section| section.id).collect();
    let questions = if section_ids.is_empty() { Vec::new() } else {
        survey_question::Entity::find()
            .filter(survey_question::Column::TenantId.eq(tenant_id))
            .filter(survey_question::Column::SectionId.is_in(section_ids))
            .order_by_asc(survey_question::Column::DisplayOrder).all(db).await?
    };
    let question_ids: Vec<Uuid> = questions.iter().map(|question| question.id).collect();
    let options = if question_ids.is_empty() { Vec::new() } else {
        survey_question_option::Entity::find()
            .filter(survey_question_option::Column::TenantId.eq(tenant_id))
            .filter(survey_question_option::Column::QuestionId.is_in(question_ids.clone())).all(db).await?
    };
    let answers = survey_answer::Entity::find()
        .filter(survey_answer::Column::TenantId.eq(tenant_id))
        .filter(survey_answer::Column::SurveyResponseId.is_in(response_ids))
        .filter(survey_answer::Column::QuestionId.is_in(question_ids))
        .all(db).await?;
    let option_by_id: HashMap<Uuid, survey_question_option::Model> = options.iter().cloned().map(|option| (option.id, option)).collect();
    let mut dimension_scores: HashMap<String, Vec<Decimal>> = HashMap::new();
    let mut aggregates = Vec::new();
    for question in questions {
        let question_answers: Vec<&survey_answer::Model> = answers.iter().filter(|answer| answer.question_id == question.id).collect();
        if !report_group_is_visible(question_answers.len(), threshold) {
            aggregates.push(SurveyQuestionAggregateDto { question_id: question.id.to_string().into(), prompt: question.prompt,
                dimension: question.dimension, response_count: 0, average_score: None, options: Vec::new(), comments: Vec::new() });
            continue;
        }
        let mut scores = Vec::new();
        let mut option_counts: HashMap<Uuid, i32> = HashMap::new();
        let mut comments = Vec::new();
        for answer in &question_answers {
            if let Some(value) = answer.numeric_answer { scores.push(value); }
            if let Some(text) = answer.text_answer.as_ref() { comments.push(text.clone()); }
            let ids = selected_ids(&answer.selected_option_ids);
            let selected_scores: Vec<Decimal> = ids.iter().filter_map(|id| {
                *option_counts.entry(*id).or_insert(0) += 1;
                option_by_id.get(id).and_then(|option| option.score)
            }).collect();
            if !selected_scores.is_empty() {
                scores.push(selected_scores.iter().copied().sum::<Decimal>() / Decimal::from(selected_scores.len() as u64));
            }
        }
        dimension_scores.entry(question.dimension.clone()).or_default().extend(scores.iter().copied());
        let average = (!scores.is_empty()).then(|| (scores.iter().copied().sum::<Decimal>() / Decimal::from(scores.len() as u64)).round_dp(2).to_string());
        let mut option_aggregates: Vec<SurveyOptionAggregateDto> = options.iter().filter(|option| option.question_id == question.id)
            .map(|option| SurveyOptionAggregateDto { option_id: option.id.to_string().into(), label: option.label.clone(), response_count: option_counts.get(&option.id).copied().unwrap_or(0) }).collect();
        option_aggregates.sort_by_key(|option| option.label.clone());
        if comments.len() < threshold { comments.clear(); }
        aggregates.push(SurveyQuestionAggregateDto { question_id: question.id.to_string().into(), prompt: question.prompt,
            dimension: question.dimension, response_count: question_answers.len() as i32, average_score: average,
            options: option_aggregates, comments });
    }
    let mut dimensions: Vec<SurveyDimensionAggregateDto> = dimension_scores.into_iter().map(|(dimension, scores)| {
        if !report_group_is_visible(scores.len(), threshold) {
            SurveyDimensionAggregateDto { dimension, scored_answer_count: 0, average_score: None }
        } else {
            let average = (scores.iter().copied().sum::<Decimal>() / Decimal::from(scores.len() as u64)).round_dp(2).to_string();
            SurveyDimensionAggregateDto { dimension, scored_answer_count: scores.len() as i32, average_score: Some(average) }
        }
    }).collect();
    dimensions.sort_by(|left, right| left.dimension.cmp(&right.dimension));
    Ok(SurveyResultsDto { survey_id: survey_id.to_string().into(), suppressed: false,
        respondent_count: Some(responses.len() as i32), minimum_report_group_size: threshold as i32,
        dimensions, questions: aggregates })
}
