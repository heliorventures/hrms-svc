//! Tenant-scoped survey persistence and aggregate reporting.

use std::collections::{HashMap, HashSet};

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
    Ok(aggregate_answers(survey_id, responses.len(), threshold, questions, options, answers))
}

// Persistence has already applied tenant and report-scope filters. Keep aggregation
// separate so privacy behavior can be tested against real answer models offline.
fn aggregate_answers(
    survey_id: Uuid,
    respondent_count: usize,
    threshold: usize,
    questions: Vec<survey_question::Model>,
    options: Vec<survey_question_option::Model>,
    answers: Vec<survey_answer::Model>,
) -> SurveyResultsDto {
    let option_by_id: HashMap<Uuid, survey_question_option::Model> = options.iter().cloned().map(|option| (option.id, option)).collect();
    let mut dimension_scores: HashMap<String, Vec<(Uuid, Decimal)>> = HashMap::new();
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
            if let Some(value) = answer.numeric_answer { scores.push((answer.survey_response_id, value)); }
            if let Some(text) = answer.text_answer.as_ref() { comments.push(text.clone()); }
            let ids = selected_ids(&answer.selected_option_ids);
            let selected_scores: Vec<Decimal> = ids.iter().filter_map(|id| {
                *option_counts.entry(*id).or_insert(0) += 1;
                option_by_id.get(id).and_then(|option| option.score)
            }).collect();
            if !selected_scores.is_empty() {
                scores.push((answer.survey_response_id,
                    selected_scores.iter().copied().sum::<Decimal>() / Decimal::from(selected_scores.len() as u64)));
            }
        }
        // Withhold the whole breakdown, including derived scores, when a small
        // bucket could be recovered by subtracting the other published values.
        let options_visible = option_counts.values().all(|count| *count as usize >= threshold);
        if !options_visible { scores.clear(); }
        dimension_scores.entry(question.dimension.clone()).or_default().extend(scores.iter().copied());
        let average = visible_score_average(&scores, threshold);
        let mut option_aggregates: Vec<SurveyOptionAggregateDto> = options.iter().filter(|option| option.question_id == question.id)
            .map(|option| SurveyOptionAggregateDto { option_id: option.id.to_string().into(), label: option.label.clone(), response_count: option_counts.get(&option.id).copied().unwrap_or(0) }).collect();
        option_aggregates.sort_by_key(|option| option.label.clone());
        if !options_visible { option_aggregates.clear(); }
        if comments.len() < threshold { comments.clear(); }
        aggregates.push(SurveyQuestionAggregateDto { question_id: question.id.to_string().into(), prompt: question.prompt,
            dimension: question.dimension, response_count: question_answers.len() as i32, average_score: average,
            options: option_aggregates, comments });
    }
    let mut dimensions: Vec<SurveyDimensionAggregateDto> = dimension_scores.into_iter().map(|(dimension, scores)| {
        let average_score = visible_score_average(&scores, threshold);
        let scored_answer_count = if average_score.is_some() { scores.len() as i32 } else { 0 };
        SurveyDimensionAggregateDto { dimension, scored_answer_count, average_score }
    }).collect();
    dimensions.sort_by(|left, right| left.dimension.cmp(&right.dimension));
    SurveyResultsDto { survey_id: survey_id.to_string().into(), suppressed: false,
        respondent_count: Some(respondent_count as i32), minimum_report_group_size: threshold as i32,
        dimensions, questions: aggregates }
}

fn visible_score_average(scores: &[(Uuid, Decimal)], threshold: usize) -> Option<String> {
    let contributors: HashSet<Uuid> = scores.iter().map(|(response_id, _)| *response_id).collect();
    report_group_is_visible(contributors.len(), threshold).then(|| {
        (scores.iter().map(|(_, score)| *score).sum::<Decimal>() / Decimal::from(scores.len() as u64))
            .round_dp(2).to_string()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn question() -> survey_question::Model {
        survey_question::Model {
            id: Uuid::new_v4(), tenant_id: Uuid::nil(), section_id: Uuid::nil(),
            dimension: "Wellbeing".into(), question_type: "SINGLE_CHOICE".into(),
            prompt: "How was your week?".into(), is_required: false,
            rating_min: None, rating_max: None, display_order: 0,
        }
    }

    fn option(question_id: Uuid, score: Option<Decimal>) -> survey_question_option::Model {
        survey_question_option::Model {
            id: Uuid::new_v4(), tenant_id: Uuid::nil(), question_id,
            label: "Choice".into(), score, display_order: 0,
        }
    }

    fn answer(question_id: Uuid, response_id: Uuid, option_id: Uuid) -> survey_answer::Model {
        survey_answer::Model {
            id: Uuid::new_v4(), tenant_id: Uuid::nil(), survey_response_id: response_id,
            question_id, selected_option_ids: Some(serde_json::json!([option_id])),
            numeric_answer: None, text_answer: None,
        }
    }

    #[test]
    fn question_score_requires_enough_scored_respondents() {
        let mut question = question();
        question.question_type = "RATING".into();
        let answers = (0..3).map(|index| {
            let mut response = answer(question.id, Uuid::new_v4(), Uuid::nil());
            response.selected_option_ids = None;
            response.numeric_answer = (index == 0).then_some(Decimal::ONE);
            response
        }).collect();
        let result = aggregate_answers(Uuid::new_v4(), 3, 3,
            vec![question], vec![], answers);
        assert_eq!(result.questions[0].response_count, 3);
        assert_eq!(result.questions[0].average_score, None);
        assert_eq!(result.dimensions[0].average_score, None);
    }

    #[test]
    fn dimension_threshold_counts_people_not_answers() {
        let respondents = [Uuid::new_v4(), Uuid::new_v4(), Uuid::new_v4()];
        let mut questions = Vec::new();
        let mut answers = Vec::new();
        for _ in 0..3 {
            let mut question = question();
            question.question_type = "RATING".into();
            for (index, respondent) in respondents.iter().enumerate() {
                let mut response = answer(question.id, *respondent, Uuid::nil());
                response.selected_option_ids = None;
                response.numeric_answer = (index == 0).then_some(Decimal::ONE);
                answers.push(response);
            }
            questions.push(question);
        }
        let result = aggregate_answers(Uuid::new_v4(), 3, 3, questions, vec![], answers);
        assert_eq!(result.dimensions[0].average_score, None);
        assert_eq!(result.dimensions[0].scored_answer_count, 0);
    }

    #[test]
    fn small_option_bucket_suppresses_entire_breakdown() {
        let question = question();
        let first = option(question.id, Some(Decimal::ONE));
        let second = option(question.id, Some(Decimal::new(5, 0)));
        let answers = [first.id, second.id, second.id, second.id].into_iter()
            .map(|id| answer(question.id, Uuid::new_v4(), id)).collect();
        let result = aggregate_answers(Uuid::new_v4(), 4, 3,
            vec![question], vec![first, second], answers);
        assert!(result.questions[0].options.is_empty());
        assert_eq!(result.questions[0].average_score, None);
        assert_eq!(result.dimensions[0].average_score, None);
    }

    #[test]
    fn qualifying_score_and_option_breakdown_remain_available() {
        let question = question();
        let choice = option(question.id, Some(Decimal::new(4, 0)));
        let answers = (0..3).map(|_| answer(question.id, Uuid::new_v4(), choice.id)).collect();
        let result = aggregate_answers(Uuid::new_v4(), 3, 3,
            vec![question], vec![choice], answers);
        assert_eq!(result.questions[0].average_score, Some("4".into()));
        assert_eq!(result.dimensions[0].average_score, Some("4".into()));
        assert_eq!(result.dimensions[0].scored_answer_count, 3);
        assert_eq!(result.questions[0].options[0].response_count, 3);
    }
}
