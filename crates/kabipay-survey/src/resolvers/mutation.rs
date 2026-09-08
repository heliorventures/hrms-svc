use std::collections::{HashMap, HashSet};

use async_graphql::{Context, InputObject, Object, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::{
    d0006_org_hierarchy::department,
    d0007_employee_core::employee,
    d0076_anonymous_surveys::{
        survey, survey_answer, survey_assignment, survey_audience_department, survey_question,
        survey_question_option, survey_response, survey_section,
    },
};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect, Set,
    TransactionTrait,
};
use uuid::Uuid;

use super::types::SurveyDto;
use crate::services::{survey_rules, survey_service};

fn parse_id(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str()).map_err(|_| KabiPayError::Validation("Invalid ID".into()).into_graphql())
}

fn text(value: &str, maximum: usize, field: &str) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > maximum {
        return Err(KabiPayError::Validation(format!("{field} must contain 1 to {maximum} characters")).into_graphql());
    }
    Ok(value.to_owned())
}

fn optional_text(value: Option<String>, maximum: usize, field: &str) -> Result<Option<String>> {
    value.filter(|value| !value.trim().is_empty()).map(|value| text(&value, maximum, field)).transpose()
}

fn decimal(value: &str, field: &str) -> Result<Decimal> {
    value.trim().parse().map_err(|_| KabiPayError::Validation(format!("{field} must be a valid number")).into_graphql())
}

#[derive(InputObject, Clone)]
pub struct SurveyQuestionOptionInput {
    pub label: String,
    pub score: Option<String>,
}

#[derive(InputObject, Clone)]
pub struct SurveyQuestionInput {
    pub dimension: String,
    pub question_type: String,
    pub prompt: String,
    #[graphql(default = false)]
    pub is_required: bool,
    pub rating_min: Option<String>,
    pub rating_max: Option<String>,
    #[graphql(default)]
    pub options: Vec<SurveyQuestionOptionInput>,
}

#[derive(InputObject, Clone)]
pub struct SurveySectionInput {
    pub title: String,
    pub questions: Vec<SurveyQuestionInput>,
}

#[derive(InputObject)]
pub struct SaveSurveyInput {
    pub id: Option<ID>,
    pub title: String,
    pub description: Option<String>,
    pub opens_at: Option<chrono::DateTime<chrono::Utc>>,
    pub closes_at: Option<chrono::DateTime<chrono::Utc>>,
    pub minimum_report_group_size: i32,
    #[graphql(default)]
    pub audience_department_ids: Vec<ID>,
    pub sections: Vec<SurveySectionInput>,
}

#[derive(InputObject)]
pub struct SurveyAnswerInput {
    pub question_id: ID,
    #[graphql(default)]
    pub selected_option_ids: Vec<ID>,
    pub numeric_answer: Option<String>,
    pub text_answer: Option<String>,
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn save_survey(&self, ctx: &Context<'_>, input: SaveSurveyInput) -> Result<SurveyDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() {
            return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let title = text(&input.title, 255, "Survey title")?;
        let description = optional_text(input.description, 4000, "Survey description")?;
        if input.minimum_report_group_size < 3 {
            return Err(KabiPayError::Validation("Minimum report group size must be at least 3".into()).into_graphql());
        }
        if input.opens_at.zip(input.closes_at).is_some_and(|(opens, closes)| closes <= opens) {
            return Err(KabiPayError::Validation("Survey close time must follow its open time".into()).into_graphql());
        }
        if input.sections.is_empty() {
            return Err(KabiPayError::Validation("At least one survey section is required".into()).into_graphql());
        }
        let mut validated_questions = Vec::new();
        for section in &input.sections {
            text(&section.title, 255, "Section title")?;
            if section.questions.is_empty() {
                return Err(KabiPayError::Validation("Every survey section requires at least one question".into()).into_graphql());
            }
            for question in &section.questions {
                text(&question.dimension, 100, "Question dimension")?;
                text(&question.prompt, 4000, "Question prompt")?;
                let question_type = match question.question_type.trim().to_ascii_uppercase().as_str() {
                    "SINGLE_CHOICE" => survey_rules::QuestionType::SingleChoice,
                    "MULTIPLE_CHOICE" => survey_rules::QuestionType::MultipleChoice,
                    "RATING" => survey_rules::QuestionType::Rating,
                    "SHORT_TEXT" => survey_rules::QuestionType::ShortText,
                    "LONG_TEXT" => survey_rules::QuestionType::LongText,
                    _ => return Err(KabiPayError::Validation("Unsupported survey question type".into()).into_graphql()),
                };
                let rating_min = question.rating_min.as_deref().map(|value| decimal(value, "Rating minimum")).transpose()?;
                let rating_max = question.rating_max.as_deref().map(|value| decimal(value, "Rating maximum")).transpose()?;
                survey_rules::validate_question(&survey_rules::QuestionRule {
                    question_type, option_count: question.options.len(), rating_min, rating_max,
                }).map_err(|message| KabiPayError::Validation(message).into_graphql())?;
                for option in &question.options {
                    text(&option.label, 500, "Option label")?;
                    if let Some(score) = option.score.as_deref() { decimal(score, "Option score")?; }
                }
                validated_questions.push((rating_min, rating_max));
            }
        }
        let audience_ids: Vec<Uuid> = input.audience_department_ids.iter().map(parse_id).collect::<Result<_>>()?;
        if audience_ids.iter().copied().collect::<HashSet<_>>().len() != audience_ids.len() {
            return Err(KabiPayError::Validation("Audience departments must be unique".into()).into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if !audience_ids.is_empty() {
            let tenant_departments = department::Entity::find()
                .filter(department::Column::TenantId.eq(tenant_id))
                .filter(department::Column::Id.is_in(audience_ids.iter().copied()))
                .filter(department::Column::IsDeleted.eq(false))
                .all(&txn)
                .await
                .map_err(KabiPayError::from)
                .map_err(KabiPayError::into_graphql)?;
            if tenant_departments.len() != audience_ids.len() {
                return Err(KabiPayError::Validation(
                    "Every survey audience department must be active and belong to this tenant".into(),
                )
                .into_graphql());
            }
        }
        let existing = match input.id {
            Some(id) => {
                let id = parse_id(&id)?;
                Some(survey::Entity::find_by_id(id).filter(survey::Column::TenantId.eq(tenant_id)).lock_exclusive()
                    .one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
                    .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: id.to_string() }.into_graphql())?)
            }
            None => None,
        };
        if existing.as_ref().is_some_and(|row| row.status != "DRAFT") {
            return Err(KabiPayError::Validation("Only draft surveys can be edited".into()).into_graphql());
        }
        let survey_id = existing.as_ref().map(|row| row.id).unwrap_or_else(Uuid::new_v4);
        if let Some(row) = existing {
            let mut model = row.into_active_model();
            model.title = Set(title.clone()); model.description = Set(description.clone());
            model.opens_at = Set(input.opens_at); model.closes_at = Set(input.closes_at);
            model.minimum_report_group_size = Set(input.minimum_report_group_size);
            model.updated_at = Set(chrono::Utc::now());
            model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            survey_section::Entity::delete_many().filter(survey_section::Column::TenantId.eq(tenant_id))
                .filter(survey_section::Column::SurveyId.eq(survey_id)).exec(&txn).await
                .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            survey_audience_department::Entity::delete_many().filter(survey_audience_department::Column::TenantId.eq(tenant_id)).filter(survey_audience_department::Column::SurveyId.eq(survey_id))
                .exec(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        } else {
            survey::ActiveModel { id: Set(survey_id), tenant_id: Set(tenant_id), title: Set(title), description: Set(description),
                status: Set("DRAFT".into()), opens_at: Set(input.opens_at), closes_at: Set(input.closes_at),
                minimum_report_group_size: Set(input.minimum_report_group_size), created_by: Set(claims.sub),
                published_at: Set(None), closed_at: Set(None), created_at: Set(chrono::Utc::now()), updated_at: Set(chrono::Utc::now()) }
                .insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        }
        for department_id in audience_ids {
            survey_audience_department::ActiveModel { tenant_id: Set(tenant_id), survey_id: Set(survey_id), department_id: Set(department_id) }
                .insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        }
        let mut validated_index = 0usize;
        for (section_index, section) in input.sections.iter().enumerate() {
            let section_id = Uuid::new_v4();
            survey_section::ActiveModel { id: Set(section_id), tenant_id: Set(tenant_id), survey_id: Set(survey_id),
                title: Set(section.title.trim().to_owned()), display_order: Set(section_index as i32) }
                .insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            for (question_index, question) in section.questions.iter().enumerate() {
                let question_id = Uuid::new_v4();
                let (rating_min, rating_max) = validated_questions[validated_index];
                validated_index += 1;
                survey_question::ActiveModel { id: Set(question_id), tenant_id: Set(tenant_id), section_id: Set(section_id),
                    dimension: Set(question.dimension.trim().to_owned()), question_type: Set(question.question_type.trim().to_ascii_uppercase()),
                    prompt: Set(question.prompt.trim().to_owned()), is_required: Set(question.is_required), rating_min: Set(rating_min),
                    rating_max: Set(rating_max), display_order: Set(question_index as i32) }
                    .insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
                for (option_index, option) in question.options.iter().enumerate() {
                    survey_question_option::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), question_id: Set(question_id),
                        label: Set(option.label.trim().to_owned()), score: Set(option.score.as_deref().map(|value| value.trim().parse()).transpose()
                            .map_err(|_| KabiPayError::Validation("Option score must be a valid number".into()).into_graphql())?),
                        display_order: Set(option_index as i32) }.insert(&txn).await
                        .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
                }
            }
        }
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        survey_service::load_survey(&db, tenant_id, survey_id, false).await.map_err(KabiPayError::into_graphql)
    }

    async fn publish_survey(&self, ctx: &Context<'_>, survey_id: ID) -> Result<SurveyDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() {
            return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let row = survey::Entity::find_by_id(survey_id).filter(survey::Column::TenantId.eq(tenant_id)).lock_exclusive()
            .one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: survey_id.to_string() }.into_graphql())?;
        if row.status != "DRAFT" { return Err(KabiPayError::Validation("Only draft surveys can be published".into()).into_graphql()); }
        let audience = survey_audience_department::Entity::find().filter(survey_audience_department::Column::TenantId.eq(tenant_id)).filter(survey_audience_department::Column::SurveyId.eq(survey_id))
            .all(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let audience_ids: Vec<Uuid> = audience.into_iter().map(|row| row.department_id).collect();
        let mut employees = employee::Entity::find().filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false)).filter(employee::Column::UserId.is_not_null())
            .filter(employee::Column::Status.is_in(["ACTIVE", "PROBATION", "ON_LEAVE"]));
        if !audience_ids.is_empty() { employees = employees.filter(employee::Column::DepartmentId.is_in(audience_ids)); }
        let employees = employees.all(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if employees.is_empty() { return Err(KabiPayError::Validation("No active employee-linked accounts match this survey audience".into()).into_graphql()); }
        for employee in employees {
            survey_assignment::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), survey_id: Set(survey_id),
                employee_id: Set(employee.id), completed_at: Set(None), created_at: Set(chrono::Utc::now()) }
                .insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        }
        let mut model = row.into_active_model();
        model.status = Set("PUBLISHED".into()); model.published_at = Set(Some(chrono::Utc::now())); model.updated_at = Set(chrono::Utc::now());
        model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        survey_service::load_survey(&db, tenant_id, survey_id, false).await.map_err(KabiPayError::into_graphql)
    }

    async fn close_survey(&self, ctx: &Context<'_>, survey_id: ID) -> Result<SurveyDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_surveys() { return Err(KabiPayError::Forbidden("survey:manage with ALL scope required".into()).into_graphql()); }
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let row = survey::Entity::find_by_id(survey_id)
            .filter(survey::Column::TenantId.eq(tenant_id))
            .lock_exclusive()
            .one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: survey_id.to_string() }.into_graphql())?;
        if row.status != "PUBLISHED" { return Err(KabiPayError::Validation("Only a published survey can be closed".into()).into_graphql()); }
        let mut model = row.into_active_model(); model.status = Set("CLOSED".into()); model.closed_at = Set(Some(chrono::Utc::now())); model.updated_at = Set(chrono::Utc::now());
        model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        survey_service::load_survey(&db, tenant_id, survey_id, false).await.map_err(KabiPayError::into_graphql)
    }

    async fn submit_survey(&self, ctx: &Context<'_>, survey_id: ID, answers: Vec<SurveyAnswerInput>) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_respond_to_surveys() { return Err(KabiPayError::Forbidden("survey:respond with SELF scope required".into()).into_graphql()); }
        let employee_id = claims.employee_id.ok_or_else(|| KabiPayError::Forbidden("An employee-linked account is required".into()).into_graphql())?;
        let tenant_id = require_tenant_id(ctx)?;
        let survey_id = parse_id(&survey_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let survey = survey::Entity::find_by_id(survey_id).filter(survey::Column::TenantId.eq(tenant_id)).lock_exclusive().one(&txn).await
            .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "survey", id: survey_id.to_string() }.into_graphql())?;
        let now = chrono::Utc::now();
        if survey.status != "PUBLISHED" || survey.opens_at.is_some_and(|opens| now < opens) || survey.closes_at.is_some_and(|closes| now >= closes) {
            return Err(KabiPayError::Validation("This survey is not currently open for responses".into()).into_graphql());
        }
        let assignment = survey_assignment::Entity::find().filter(survey_assignment::Column::TenantId.eq(tenant_id))
            .filter(survey_assignment::Column::SurveyId.eq(survey_id)).filter(survey_assignment::Column::EmployeeId.eq(employee_id))
            .lock_exclusive().one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::Forbidden("This survey is not assigned to the signed-in employee".into()).into_graphql())?;
        if assignment.completed_at.is_some() { return Err(KabiPayError::Conflict("This survey has already been submitted".into()).into_graphql()); }
        let sections = survey_section::Entity::find().filter(survey_section::Column::TenantId.eq(tenant_id))
            .filter(survey_section::Column::SurveyId.eq(survey_id)).all(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let section_ids: Vec<Uuid> = sections.into_iter().map(|row| row.id).collect();
        let questions = survey_question::Entity::find().filter(survey_question::Column::TenantId.eq(tenant_id))
            .filter(survey_question::Column::SectionId.is_in(section_ids)).all(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let question_map: HashMap<Uuid, survey_question::Model> = questions.into_iter().map(|row| (row.id, row)).collect();
        let mut answered = HashSet::new();
        let mut parsed = Vec::new();
        for answer in answers {
            let question_id = parse_id(&answer.question_id)?;
            if !answered.insert(question_id) { return Err(KabiPayError::Validation("Each survey question can be answered only once".into()).into_graphql()); }
            let question = question_map.get(&question_id).ok_or_else(|| KabiPayError::Validation("An answer references a question outside this survey".into()).into_graphql())?;
            let option_ids: Vec<Uuid> = answer.selected_option_ids.iter().map(parse_id).collect::<Result<_>>()?;
            let numeric = answer.numeric_answer.as_deref().map(|value| decimal(value, "Numeric answer")).transpose()?;
            let text_answer = optional_text(answer.text_answer, 8000, "Text answer")?;
            let value_count = usize::from(!option_ids.is_empty()) + usize::from(numeric.is_some()) + usize::from(text_answer.is_some());
            if value_count != 1 { return Err(KabiPayError::Validation("Each answered survey question must provide exactly one compatible value".into()).into_graphql()); }
            match question.question_type.as_str() {
                "SINGLE_CHOICE" if option_ids.len() != 1 => return Err(KabiPayError::Validation("Single-choice questions require exactly one option".into()).into_graphql()),
                "SINGLE_CHOICE" | "MULTIPLE_CHOICE" if numeric.is_some() || text_answer.is_some() => return Err(KabiPayError::Validation("Choice questions require selected options".into()).into_graphql()),
                "RATING" if numeric.is_none() => return Err(KabiPayError::Validation("Rating questions require a numeric answer".into()).into_graphql()),
                "SHORT_TEXT" | "LONG_TEXT" if text_answer.is_none() => return Err(KabiPayError::Validation("Text questions require a text answer".into()).into_graphql()),
                _ => {}
            }
            if let Some(value) = numeric {
                if value < question.rating_min.unwrap_or(value) || value > question.rating_max.unwrap_or(value) {
                    return Err(KabiPayError::Validation("A rating answer is outside the configured range".into()).into_graphql());
                }
            }
            parsed.push((question_id, option_ids, numeric, text_answer));
        }
        for question in question_map.values().filter(|question| question.is_required) {
            if !answered.contains(&question.id) { return Err(KabiPayError::Validation(format!("Required question '{}' must be answered", question.prompt)).into_graphql()); }
        }
        let all_options: Vec<Uuid> = parsed.iter().flat_map(|(_, ids, _, _)| ids.iter().copied()).collect();
        if !all_options.is_empty() {
            let valid = survey_question_option::Entity::find().filter(survey_question_option::Column::TenantId.eq(tenant_id))
                .filter(survey_question_option::Column::Id.is_in(all_options.clone())).all(&txn).await
                .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            if valid.len() != all_options.len() || valid.iter().any(|option| !parsed.iter().any(|(question_id, ids, _, _)| *question_id == option.question_id && ids.contains(&option.id))) {
                return Err(KabiPayError::Validation("A selected option does not belong to its survey question".into()).into_graphql());
            }
        }
        let employee = employee::Entity::find_by_id(employee_id).filter(employee::Column::TenantId.eq(tenant_id)).one(&txn).await
            .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::Forbidden("Employee profile is unavailable".into()).into_graphql())?;
        let response_id = Uuid::new_v4();
        survey_response::ActiveModel { id: Set(response_id), tenant_id: Set(tenant_id), survey_id: Set(survey_id),
            department_id: Set(employee.department_id), manager_employee_id: Set(employee.reporting_manager_id), submitted_at: Set(now) }
            .insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        for (question_id, option_ids, numeric, text_answer) in parsed {
            survey_answer::ActiveModel { id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), survey_response_id: Set(response_id),
                question_id: Set(question_id), selected_option_ids: Set((!option_ids.is_empty()).then(|| serde_json::Value::Array(option_ids.into_iter().map(|id| serde_json::Value::String(id.to_string())).collect()))),
                numeric_answer: Set(numeric), text_answer: Set(text_answer) }.insert(&txn).await
                .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        }
        let mut assignment = assignment.into_active_model(); assignment.completed_at = Set(Some(now));
        assignment.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn save_survey_rejects_narrow_manage_scope_before_database_access() {
        let claims: kabipay_common::context::ClientClaims = serde_json::from_value(serde_json::json!({
            "sub": Uuid::new_v4(), "tenant_id": Uuid::new_v4(), "iss": "kabipay-client",
            "iat": 0, "exp": 9999999999i64, "permissions": ["survey:manage"],
            "permission_scopes": {"survey:manage": "TEAM"}
        })).unwrap();
        let schema = async_graphql::Schema::build(crate::resolvers::QueryRoot, MutationRoot, async_graphql::EmptySubscription)
            .data(claims).finish();
        let response = schema.execute(r#"mutation { saveSurvey(input: {title: "Pulse", minimumReportGroupSize: 5, sections: []}) { summary { id } } }"#).await;
        assert_eq!(response.errors.len(), 1);
        assert!(response.errors[0].message.contains("ALL scope required"));
    }
}
