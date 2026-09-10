//! GraphQL survey response types. No type in this module exposes a respondent identity.

use async_graphql::{SimpleObject, ID};
use chrono::{DateTime, Utc};
use kabipay_db_entities::tenant::d0076_anonymous_surveys::{
    survey, survey_question, survey_question_option, survey_section,
};

/// Static management history; deliberately excludes respondent and actor identities.
#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyManagementEvent")]
pub struct SurveyManagementEventDto {
    pub action: String,
    pub occurred_at: DateTime<Utc>,
    pub message: String,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveySummary")]
pub struct SurveySummaryDto {
    pub id: ID,
    pub title: String,
    pub description: Option<String>,
    pub status: String,
    pub opens_at: Option<DateTime<Utc>>,
    pub closes_at: Option<DateTime<Utc>>,
    pub minimum_report_group_size: i32,
    pub completed: bool,
    pub response_review_mode: String,
    pub assigned_count: Option<i32>,
    pub completed_count: Option<i32>,
    pub pending_count: Option<i32>,
}

impl SurveySummaryDto {
    pub fn from_model(model: survey::Model, completed: bool) -> Self {
        Self {
            id: model.id.to_string().into(),
            title: model.title,
            description: model.description,
            status: model.status,
            opens_at: model.opens_at,
            closes_at: model.closes_at,
            minimum_report_group_size: model.minimum_report_group_size,
            completed,
            response_review_mode: model.response_review_mode,
            assigned_count: None, completed_count: None, pending_count: None,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyQuestionOption")]
pub struct SurveyQuestionOptionDto {
    pub id: ID,
    pub label: String,
    pub score: Option<String>,
    pub display_order: i32,
}

impl From<survey_question_option::Model> for SurveyQuestionOptionDto {
    fn from(model: survey_question_option::Model) -> Self {
        Self {
            id: model.id.to_string().into(),
            label: model.label,
            score: model.score.map(|value| value.to_string()),
            display_order: model.display_order,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyQuestion")]
pub struct SurveyQuestionDto {
    pub id: ID,
    pub dimension: String,
    pub question_type: String,
    pub prompt: String,
    pub description: Option<String>,
    pub comment_enabled: bool,
    pub is_required: bool,
    pub rating_min: Option<String>,
    pub rating_max: Option<String>,
    pub display_order: i32,
    pub options: Vec<SurveyQuestionOptionDto>,
}

impl SurveyQuestionDto {
    pub fn from_model(
        model: survey_question::Model,
        options: Vec<survey_question_option::Model>,
    ) -> Self {
        Self {
            id: model.id.to_string().into(),
            dimension: model.dimension,
            question_type: model.question_type,
            prompt: model.prompt,
            description: model.description,
            comment_enabled: model.comment_enabled,
            is_required: model.is_required,
            rating_min: model.rating_min.map(|value| value.to_string()),
            rating_max: model.rating_max.map(|value| value.to_string()),
            display_order: model.display_order,
            options: options.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveySection")]
pub struct SurveySectionDto {
    pub id: ID,
    pub title: String,
    pub display_order: i32,
    pub questions: Vec<SurveyQuestionDto>,
}

impl SurveySectionDto {
    pub fn from_model(model: survey_section::Model, questions: Vec<SurveyQuestionDto>) -> Self {
        Self {
            id: model.id.to_string().into(),
            title: model.title,
            display_order: model.display_order,
            questions,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Survey")]
pub struct SurveyDto {
    pub summary: SurveySummaryDto,
    pub audience_department_ids: Vec<ID>,
    pub sections: Vec<SurveySectionDto>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyOptionAggregate")]
pub struct SurveyOptionAggregateDto {
    pub option_id: ID,
    pub label: String,
    pub response_count: i32,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyQuestionAggregate")]
pub struct SurveyQuestionAggregateDto {
    pub suppressed: bool,
    pub question_id: ID,
    pub prompt: String,
    pub dimension: String,
    pub response_count: i32,
    pub average_score: Option<String>,
    pub question_type: String,
    pub rating_min: Option<String>,
    pub rating_max: Option<String>,
    pub skipped_count: Option<i32>,
    pub rating_distribution: Vec<SurveyRatingBucket>,
    pub options: Vec<SurveyOptionAggregateDto>,
    pub comments: Vec<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyDimensionAggregate")]
pub struct SurveyDimensionAggregateDto {
    pub dimension: String,
    pub scored_answer_count: i32,
    pub average_score: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "SurveyResults")]
pub struct SurveyResultsDto {
    pub survey_id: ID,
    pub suppressed: bool,
    pub suppression_reason: Option<String>,
    pub respondent_count: Option<i32>,
    pub minimum_report_group_size: i32,
    pub dimensions: Vec<SurveyDimensionAggregateDto>,
    pub questions: Vec<SurveyQuestionAggregateDto>,
}

#[derive(SimpleObject, Clone, Debug)]
pub struct SurveyRatingBucket { pub score: String, pub response_count: i32 }

#[derive(SimpleObject)]
pub struct SurveySubmissionAnswer {
    pub question_id: ID,
    pub prompt: String,
    pub numeric_answer: Option<String>,
    pub text_answer: Option<String>,
    pub comment: Option<String>,
    pub selected_options: Vec<String>,
}
#[derive(SimpleObject)]
pub struct SurveyAnonymousSubmission { pub number: i32, pub answers: Vec<SurveySubmissionAnswer> }
#[derive(SimpleObject)]
pub struct SurveySubmissions {
    pub available: bool, pub reason: Option<String>, pub total_count: Option<i32>,
    pub nodes: Vec<SurveyAnonymousSubmission>, pub has_more: bool,
}
