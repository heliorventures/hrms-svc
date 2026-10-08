//! GraphQL DTOs for kabipay-performance.

use async_graphql::{SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_db_entities::tenant::d0018_performance::{goal, review_cycle};
use kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::{
    appraisal_answer, appraisal_question, appraisal_question_option, appraisal_template,
    appraisal_template_section, continuous_feedback, performance_participant,
    performance_program,
};

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "ReviewCycle")]
pub struct ReviewCycleDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub start_date: NaiveDate,
    pub end_date: NaiveDate,
    pub status: String,
    pub review_type: Option<String>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<review_cycle::Model> for ReviewCycleDto {
    fn from(m: review_cycle::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            start_date: m.start_date,
            end_date: m.end_date,
            status: m.status,
            review_type: m.review_type,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "Goal")]
pub struct GoalDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub review_cycle_id: ID,
    pub title: String,
    pub description: Option<String>,
    pub weightage: Option<String>,
    pub status: String,
}

impl From<goal::Model> for GoalDto {
    fn from(m: goal::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            review_cycle_id: ID(m.review_cycle_id.to_string()),
            title: m.title,
            description: m.description,
            weightage: m.weightage.map(|d| d.to_string()),
            status: m.status,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PerformanceProgram")]
pub struct PerformanceProgramDto {
    pub id: ID,
    pub name: String,
    pub description: Option<String>,
    pub cadence: String,
    pub anchor_date: NaiveDate,
    pub status: String,
    pub include_calibration: bool,
    pub include_acknowledgement: bool,
    pub goal_weight_required: String,
    pub rating_min: String,
    pub rating_max: String,
}

impl From<performance_program::Model> for PerformanceProgramDto {
    fn from(model: performance_program::Model) -> Self {
        Self {
            id: ID(model.id.to_string()),
            name: model.name,
            description: model.description,
            cadence: model.cadence,
            anchor_date: model.anchor_date,
            status: model.status,
            include_calibration: model.include_calibration,
            include_acknowledgement: model.include_acknowledgement,
            goal_weight_required: model.goal_weight_required.to_string(),
            rating_min: model.rating_min.to_string(),
            rating_max: model.rating_max.to_string(),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AppraisalQuestionOption")]
pub struct AppraisalQuestionOptionDto {
    pub id: ID,
    pub label: String,
    pub score: Option<String>,
    pub display_order: i32,
}

impl From<appraisal_question_option::Model> for AppraisalQuestionOptionDto {
    fn from(model: appraisal_question_option::Model) -> Self {
        Self {
            id: ID(model.id.to_string()),
            label: model.label,
            score: model.score.map(|value| value.to_string()),
            display_order: model.display_order,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AppraisalQuestion")]
pub struct AppraisalQuestionDto {
    pub id: ID,
    pub parent_question_id: Option<ID>,
    pub question_type: String,
    pub prompt: String,
    pub is_required: bool,
    pub answerer: String,
    pub self_rating_enabled: bool,
    pub manager_rating_enabled: bool,
    pub display_order: i32,
    pub options: Vec<AppraisalQuestionOptionDto>,
}

impl AppraisalQuestionDto {
    pub fn from_model(
        model: appraisal_question::Model,
        options: Vec<appraisal_question_option::Model>,
    ) -> Self {
        Self {
            id: ID(model.id.to_string()),
            parent_question_id: model.parent_question_id.map(|id| ID(id.to_string())),
            question_type: model.question_type,
            prompt: model.prompt,
            is_required: model.is_required,
            answerer: model.answerer,
            self_rating_enabled: model.self_rating_enabled,
            manager_rating_enabled: model.manager_rating_enabled,
            display_order: model.display_order,
            options: options.into_iter().map(Into::into).collect(),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AppraisalSection")]
pub struct AppraisalSectionDto {
    pub id: ID,
    pub title: String,
    pub description: Option<String>,
    pub display_order: i32,
    pub questions: Vec<AppraisalQuestionDto>,
}

impl AppraisalSectionDto {
    pub fn from_model(
        model: appraisal_template_section::Model,
        questions: Vec<AppraisalQuestionDto>,
    ) -> Self {
        Self {
            id: ID(model.id.to_string()),
            title: model.title,
            description: model.description,
            display_order: model.display_order,
            questions,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AppraisalTemplate")]
pub struct AppraisalTemplateDto {
    pub id: ID,
    pub performance_program_id: ID,
    pub version: i32,
    pub name: String,
    pub status: String,
    pub published_at: Option<DateTime<Utc>>,
    pub sections: Vec<AppraisalSectionDto>,
}

impl AppraisalTemplateDto {
    pub fn from_model(model: appraisal_template::Model, sections: Vec<AppraisalSectionDto>) -> Self {
        Self {
            id: ID(model.id.to_string()),
            performance_program_id: ID(model.performance_program_id.to_string()),
            version: model.version,
            name: model.name,
            status: model.status,
            published_at: model.published_at,
            sections,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PerformanceReviewSummary")]
pub struct PerformanceReviewSummaryDto {
    pub id: ID,
    pub review_cycle_id: ID,
    pub employee_id: ID,
    pub employee_name: String,
    pub manager_employee_id: Option<ID>,
    pub manager_name: Option<String>,
    pub appraisal_template_id: ID,
    pub cycle_name: String,
    pub cycle_start_date: NaiveDate,
    pub cycle_end_date: NaiveDate,
    pub cycle_stage: String,
    pub status: String,
    pub response_revision: i32,
    pub self_submitted_at: Option<DateTime<Utc>>,
    pub manager_submitted_at: Option<DateTime<Utc>>,
    pub acknowledged_at: Option<DateTime<Utc>>,
    pub final_rating: Option<String>,
    pub performance_band: Option<String>,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "AppraisalAnswer")]
pub struct AppraisalAnswerDto {
    pub question_id: ID,
    pub employee_text_answer: Option<String>,
    pub employee_selected_option_ids: Vec<ID>,
    pub self_rating: Option<String>,
    pub manager_text_answer: Option<String>,
    pub manager_selected_option_ids: Vec<ID>,
    pub manager_rating: Option<String>,
}

fn json_ids(value: Option<serde_json::Value>) -> Vec<ID> {
    value
        .and_then(|value| value.as_array().cloned())
        .unwrap_or_default()
        .into_iter()
        .filter_map(|value| value.as_str().map(|value| ID(value.to_owned())))
        .collect()
}

impl From<appraisal_answer::Model> for AppraisalAnswerDto {
    fn from(model: appraisal_answer::Model) -> Self {
        Self {
            question_id: ID(model.question_id.to_string()),
            employee_text_answer: model.employee_text_answer,
            employee_selected_option_ids: json_ids(model.employee_selected_option_ids),
            self_rating: model.self_rating.map(|value| value.to_string()),
            manager_text_answer: model.manager_text_answer,
            manager_selected_option_ids: json_ids(model.manager_selected_option_ids),
            manager_rating: model.manager_rating.map(|value| value.to_string()),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PerformanceFeedback")]
pub struct PerformanceFeedbackDto {
    pub id: ID,
    pub review_cycle_id: Option<ID>,
    pub goal_id: Option<ID>,
    pub observation_date: NaiveDate,
    pub comments: String,
    pub created_at: DateTime<Utc>,
}

impl From<continuous_feedback::Model> for PerformanceFeedbackDto {
    fn from(model: continuous_feedback::Model) -> Self {
        Self {
            id: ID(model.id.to_string()),
            review_cycle_id: model.review_cycle_id.map(|id| ID(id.to_string())),
            goal_id: model.goal_id.map(|id| ID(id.to_string())),
            observation_date: model.observation_date,
            comments: model.comments,
            created_at: model.created_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "PerformanceReviewDetail")]
pub struct PerformanceReviewDetailDto {
    pub review: PerformanceReviewSummaryDto,
    pub goals: Vec<GoalDto>,
    pub feedback: Vec<PerformanceFeedbackDto>,
    pub template: AppraisalTemplateDto,
    pub answers: Vec<AppraisalAnswerDto>,
}

pub fn participant_id(model: &performance_participant::Model) -> ID {
    ID(model.id.to_string())
}
