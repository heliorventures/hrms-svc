use async_graphql::{Context, InputObject, Object, Result, ID};
use kabipay_common::{
    context::ScopeType,
    subgraph::{require_client_claims, require_tenant_id},
    KabiPayError,
};
use kabipay_db_entities::tenant::{
    d0018_performance::{goal, kpi, review_cycle},
    d0075_performance_appraisal_lifecycle::{
        appraisal_answer, appraisal_question, appraisal_question_option, appraisal_template,
        appraisal_template_section, continuous_feedback, performance_participant,
        performance_program,
    },
};
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait,
    IntoActiveModel, PaginatorTrait, QueryFilter, QueryOrder, QuerySelect, Set,
    Statement, TransactionTrait, ModelTrait, DatabaseTransaction, TryGetable,
};
use sea_orm::prelude::Expr;
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

use super::types::{
    AppraisalTemplateDto, GoalDto, PerformanceFeedbackDto, PerformanceProgramDto,
    PerformanceReviewDetailDto, ReviewCycleDto,
};
use crate::services::{
    performance_administration, performance_lifecycle, performance_policy, performance_workflow,
};

#[cfg(not(test))]
use kabipay_common::subgraph::tenant_db;
#[cfg(test)]
use super::concurrency_tests::tenant_db;

pub(crate) fn validate_text(value: &str, maximum: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > maximum {
        return Err(KabiPayError::Validation(format!(
            "Text must contain 1 to {maximum} characters"
        )).into_graphql());
    }
    Ok(value.to_owned())
}

pub(crate) fn optional_text(value: Option<String>, maximum: usize) -> Result<Option<String>> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| validate_text(&value, maximum))
        .transpose()
}

pub(crate) fn parse_id(id: &ID) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|_| KabiPayError::Validation("Invalid ID".into()).into_graphql())
}

pub(crate) fn parse_decimal(value: &str, field: &str) -> Result<Decimal> {
    value.trim().parse::<Decimal>().map_err(|_| {
        KabiPayError::Validation(format!("{field} must be a valid number")).into_graphql()
    })
}

fn require_expected_revision(actual: i32, expected: Option<i32>) -> Result<()> {
    if expected == Some(actual) || (expected.is_none() && actual == 1) {
        Ok(())
    } else {
        Err(KabiPayError::Conflict(
            "Performance review changed; refresh before submitting this revision".into(),
        ).into_graphql())
    }
}

fn require_appraisal_submission_state(
    participant: &performance_participant::Model,
    locked_cycle_id: Uuid,
    manager_submission: bool,
) -> Result<()> {
    if participant.review_cycle_id != locked_cycle_id {
        return Err(KabiPayError::Conflict("Performance review changed while acquiring its cycle lock; retry the request".into()).into_graphql());
    }
    if participant.is_excluded {
        return Err(KabiPayError::Validation("Excluded participants cannot submit an appraisal".into()).into_graphql());
    }
    if manager_submission && participant.self_submitted_at.is_none() {
        return Err(KabiPayError::Validation("A self-appraisal is required before manager appraisal".into()).into_graphql());
    }
    Ok(())
}

pub(crate) fn require_manage(claims: &kabipay_common::context::ClientClaims) -> Result<()> {
    if claims.can_manage_performance_programs() {
        Ok(())
    } else {
        Err(KabiPayError::Forbidden(
            "performance:manage with ALL scope required".into(),
        )
        .into_graphql())
    }
}

fn require_employee_id(claims: &kabipay_common::context::ClientClaims) -> Result<Uuid> {
    claims.employee_id.ok_or_else(|| {
        KabiPayError::Forbidden("An employee-linked account is required".into()).into_graphql()
    })
}

pub(crate) fn goal_actor_can_manage(
    claims: &kabipay_common::context::ClientClaims,
    participant: &performance_participant::Model,
    own_proposal_only: bool,
    goal_status: Option<&str>,
) -> Result<()> {
    if claims.can_manage_performance_programs() {
        return Ok(());
    }
    let actor = require_employee_id(claims)?;
    if claims.can_evaluate_performance_team()
        && performance_lifecycle::manager_matches_snapshot(actor, participant.manager_employee_id)
    {
        return Ok(());
    }
    if claims.can_use_performance_self_service() && actor == participant.employee_id {
        if own_proposal_only && goal_status != Some("PROPOSED") {
            return Err(KabiPayError::Forbidden(
                "Employees can only change proposed goals".into(),
            ).into_graphql());
        }
        return Ok(());
    }
    Err(KabiPayError::Forbidden("You are not authorized to change this goal".into()).into_graphql())
}

fn require_goal_mutation_authority(claims: &kabipay_common::context::ClientClaims) -> Result<()> {
    if claims.can_manage_performance_programs() {
        return Ok(());
    }
    if (claims.can_use_performance_self_service() || claims.can_evaluate_performance_team())
        && claims.employee_id.is_some()
    {
        return Ok(());
    }
    Err(KabiPayError::Forbidden("A concrete employee-linked performance authority is required".into()).into_graphql())
}

async fn locked_goal_participant<C: ConnectionTrait>(
    txn: &C,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> Result<performance_participant::Model> {
    performance_participant::Entity::find_by_id(participant_id)
        .filter(performance_participant::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(txn)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance review",
            id: participant_id.to_string(),
        }.into_graphql())
}

pub(crate) async fn locked_goal_context(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> Result<(performance_participant::Model, String)> {
    let cycle_id = performance_participant::Entity::find_by_id(participant_id)
        .filter(performance_participant::Column::TenantId.eq(tenant_id))
        .one(txn).await
        .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
        .map(|row| row.review_cycle_id)
        .ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?;
    let stage = performance_workflow::locked_cycle_stage(txn, tenant_id, cycle_id).await
        .map_err(KabiPayError::into_graphql)?;
    let participant = locked_goal_participant(txn, tenant_id, participant_id).await?;
    if participant.review_cycle_id != cycle_id {
        return Err(KabiPayError::Conflict(
            "Performance review changed while acquiring its cycle lock; retry the request".into(),
        )
        .into_graphql());
    }
    Ok((participant, stage))
}

fn require_goal_setting(participant: &performance_participant::Model, stage: &str) -> Result<()> {
    if participant.is_excluded {
        return Err(KabiPayError::Validation("Excluded participants cannot have goals".into()).into_graphql());
    }
    if stage != "GOAL_SETTING" {
        return Err(KabiPayError::Validation("Goals can only be changed during goal setting".into()).into_graphql());
    }
    Ok(())
}

#[derive(InputObject)]
pub struct SaveReviewCycleInput {
    pub id: Option<ID>,
    pub name: String,
    pub start_date: chrono::NaiveDate,
    pub end_date: chrono::NaiveDate,
    pub review_type: Option<String>,
}

#[derive(InputObject)]
pub struct SavePerformanceProgramInput {
    pub id: Option<ID>,
    pub name: String,
    pub description: Option<String>,
    pub cadence: String,
    pub anchor_date: chrono::NaiveDate,
    #[graphql(default = false)]
    pub include_calibration: bool,
    #[graphql(default = true)]
    pub include_acknowledgement: bool,
    pub goal_weight_required: String,
    pub rating_min: String,
    pub rating_max: String,
}

#[derive(InputObject, Clone)]
pub struct AppraisalQuestionOptionInput {
    pub label: String,
    pub score: Option<String>,
}

#[derive(InputObject, Clone)]
pub struct AppraisalQuestionInput {
    pub client_key: String,
    pub parent_client_key: Option<String>,
    pub question_type: String,
    pub prompt: String,
    #[graphql(default = false)]
    pub is_required: bool,
    pub answerer: String,
    #[graphql(default = false)]
    pub self_rating_enabled: bool,
    #[graphql(default = false)]
    pub manager_rating_enabled: bool,
    #[graphql(default)]
    pub options: Vec<AppraisalQuestionOptionInput>,
}

#[derive(InputObject, Clone)]
pub struct AppraisalSectionInput {
    pub title: String,
    pub description: Option<String>,
    pub questions: Vec<AppraisalQuestionInput>,
}

#[derive(InputObject)]
pub struct SaveAppraisalTemplateInput {
    pub id: Option<ID>,
    pub performance_program_id: ID,
    pub name: String,
    pub sections: Vec<AppraisalSectionInput>,
}

#[derive(InputObject)]
pub struct LaunchPerformanceCycleInput {
    pub performance_program_id: ID,
    pub appraisal_template_id: ID,
    pub period_date: chrono::NaiveDate,
    pub self_review_due_date: Option<chrono::NaiveDate>,
    pub manager_review_due_date: Option<chrono::NaiveDate>,
}

#[derive(InputObject)]
pub struct SavePerformanceGoalInput {
    pub participant_id: ID,
    pub title: String,
    pub description: Option<String>,
    pub weightage: String,
}

#[derive(InputObject)]
pub struct AddPerformanceFeedbackInput {
    pub participant_id: ID,
    pub goal_id: Option<ID>,
    pub observation_date: chrono::NaiveDate,
    pub comments: String,
}

#[derive(InputObject, Clone)]
pub struct AppraisalAnswerInput {
    pub question_id: ID,
    pub text_answer: Option<String>,
    #[graphql(default)]
    pub selected_option_ids: Vec<ID>,
    pub rating: Option<String>,
}

pub struct MutationRoot;

#[Object(name = "PerformanceMutationOperations")]
impl MutationRoot {
    async fn save_review_cycle(
        &self,
        ctx: &Context<'_>,
        input: SaveReviewCycleInput,
    ) -> Result<ReviewCycleDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["performance:manage"])
            || claims.explicit_scope_for_permission("performance:manage") != Some(ScopeType::All)
        {
            return Err(KabiPayError::Forbidden(
                "performance:manage with ALL scope required".into(),
            ).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let name = validate_text(&input.name, 255)?;
        let review_type = optional_text(input.review_type, 50)?;
        if input.end_date < input.start_date {
            return Err(KabiPayError::Validation(
                "End date must be on or after start date".into(),
            ).into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let existing = match input.id {
            Some(id) => {
                let id = Uuid::parse_str(id.as_str()).map_err(|_| {
                    KabiPayError::Validation("Invalid ID".into()).into_graphql()
                })?;
                Some(review_cycle::Entity::find_by_id(id)
                    .filter(review_cycle::Column::TenantId.eq(tenant_id))
                    .lock_exclusive()
                    .one(&txn)
                    .await
                    .map_err(KabiPayError::from)
                    .map_err(KabiPayError::into_graphql)?
                    .ok_or_else(|| KabiPayError::NotFound {
                        entity: "catalog record",
                        id: id.to_string(),
                    }.into_graphql())?)
            }
            None => None,
        };
        if existing.as_ref().is_some_and(|row| !row.status.eq_ignore_ascii_case("DRAFT")) {
            return Err(KabiPayError::Validation(
                "Only draft review cycles can be edited".into(),
            ).into_graphql());
        }
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| review_cycle::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.start_date = Set(input.start_date);
        model.end_date = Set(input.end_date);
        model.review_type = Set(review_type);
        if is_new {
            model.status = Set("DRAFT".into());
        }
        model.updated_at = Set(chrono::Utc::now());
        let row = if is_new {
            model.insert(&txn).await
        } else {
            model.update(&txn).await
        }.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(row.into())
    }

    async fn save_performance_program(
        &self,
        ctx: &Context<'_>,
        input: SavePerformanceProgramInput,
    ) -> Result<PerformanceProgramDto> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let name = validate_text(&input.name, 255)?;
        let description = optional_text(input.description, 4000)?;
        let cadence = input.cadence.trim().to_ascii_uppercase();
        if !["MONTHLY", "QUARTERLY", "YEARLY", "MANUAL"].contains(&cadence.as_str()) {
            return Err(KabiPayError::Validation(
                "Cadence must be MONTHLY, QUARTERLY, YEARLY, or MANUAL".into(),
            )
            .into_graphql());
        }
        let goal_weight_required = parse_decimal(&input.goal_weight_required, "Goal weight")?;
        let rating_min = parse_decimal(&input.rating_min, "Minimum rating")?;
        let rating_max = parse_decimal(&input.rating_max, "Maximum rating")?;
        if goal_weight_required < Decimal::ZERO || goal_weight_required > Decimal::ONE_HUNDRED {
            return Err(KabiPayError::Validation(
                "Goal weight requirement must be between 0 and 100".into(),
            )
            .into_graphql());
        }
        performance_lifecycle::validate_rating(rating_min, rating_min, rating_max)
            .map_err(|message| KabiPayError::Validation(message).into_graphql())?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let existing = match input.id {
            Some(id) => {
                let id = parse_id(&id)?;
                Some(
                    performance_program::Entity::find_by_id(id)
                        .filter(performance_program::Column::TenantId.eq(tenant_id))
                        .lock_exclusive()
                        .one(&txn)
                        .await
                        .map_err(KabiPayError::from)
                        .map_err(KabiPayError::into_graphql)?
                        .ok_or_else(|| KabiPayError::NotFound {
                            entity: "performance program",
                            id: id.to_string(),
                        }
                        .into_graphql())?,
                )
            }
            None => None,
        };
        if existing.as_ref().is_some_and(|row| row.status != "DRAFT") {
            return Err(KabiPayError::Validation(
                "Only draft performance programs can be edited".into(),
            )
            .into_graphql());
        }
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| performance_program::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                status: Set("DRAFT".into()),
                created_by: Set(Some(claims.sub)),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.description = Set(description);
        model.cadence = Set(cadence);
        model.anchor_date = Set(input.anchor_date);
        model.include_calibration = Set(input.include_calibration);
        model.include_acknowledgement = Set(input.include_acknowledgement);
        model.goal_weight_required = Set(goal_weight_required);
        model.rating_min = Set(rating_min);
        model.rating_max = Set(rating_max);
        model.updated_at = Set(chrono::Utc::now());
        let saved = if is_new { model.insert(&txn).await } else { model.update(&txn).await }
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
    }

    async fn save_appraisal_template(
        &self,
        ctx: &Context<'_>,
        input: SaveAppraisalTemplateInput,
    ) -> Result<AppraisalTemplateDto> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let program_id = parse_id(&input.performance_program_id)?;
        let name = validate_text(&input.name, 255)?;
        if input.sections.is_empty() {
            return Err(KabiPayError::Validation(
                "At least one appraisal section is required".into(),
            )
            .into_graphql());
        }
        let mut rules = Vec::new();
        let mut question_keys = HashSet::new();
        for section in &input.sections {
            validate_text(&section.title, 255)?;
            if section.questions.is_empty() {
                return Err(KabiPayError::Validation(
                    "Every appraisal section requires at least one question".into(),
                )
                .into_graphql());
            }
            let section_keys: HashSet<&str> = section
                .questions
                .iter()
                .map(|question| question.client_key.trim())
                .collect();
            for question in &section.questions {
                let key = validate_text(&question.client_key, 100)?;
                if !question_keys.insert(key.clone()) {
                    return Err(KabiPayError::Validation(
                        "Question client keys must be unique".into(),
                    )
                    .into_graphql());
                }
                if question.parent_client_key.as_deref().is_some_and(|parent| !section_keys.contains(parent.trim())) {
                    return Err(KabiPayError::Validation(
                        "A subquestion must reference a question in the same section".into(),
                    )
                    .into_graphql());
                }
                validate_text(&question.prompt, 4000)?;
                let question_type = question.question_type.trim().to_ascii_uppercase();
                let lifecycle_type = match question_type.as_str() {
                    "SINGLE_CHOICE" | "MULTIPLE_CHOICE" => performance_lifecycle::QuestionType::MultipleChoice,
                    "RATING" => performance_lifecycle::QuestionType::Rating,
                    "SHORT_TEXT" => performance_lifecycle::QuestionType::ShortText,
                    "LONG_TEXT" => performance_lifecycle::QuestionType::LongText,
                    _ => return Err(KabiPayError::Validation(
                        "Unsupported appraisal question type".into(),
                    ).into_graphql()),
                };
                let answerer = question.answerer.trim().to_ascii_uppercase();
                if !["EMPLOYEE", "MANAGER", "BOTH"].contains(&answerer.as_str()) {
                    return Err(KabiPayError::Validation(
                        "Question answerer must be EMPLOYEE, MANAGER, or BOTH".into(),
                    ).into_graphql());
                }
                for option in &question.options {
                    validate_text(&option.label, 500)?;
                    if let Some(score) = option.score.as_deref() {
                        parse_decimal(score, "Option score")?;
                    }
                }
                rules.push(performance_lifecycle::QuestionRule {
                    id: key,
                    parent_id: question.parent_client_key.as_ref().map(|value| value.trim().to_owned()),
                    question_type: lifecycle_type,
                    option_count: question.options.len(),
                });
            }
        }
        performance_lifecycle::validate_questionnaire(&rules)
            .map_err(|message| KabiPayError::Validation(message).into_graphql())?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_program::Entity::find_by_id(program_id)
            .filter(performance_program::Column::TenantId.eq(tenant_id))
            .one(&txn).await
            .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance program", id: program_id.to_string() }.into_graphql())?;
        let existing = match input.id {
            Some(id) => {
                let id = parse_id(&id)?;
                Some(appraisal_template::Entity::find_by_id(id)
                    .filter(appraisal_template::Column::TenantId.eq(tenant_id))
                    .lock_exclusive().one(&txn).await
                    .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
                    .ok_or_else(|| KabiPayError::NotFound { entity: "appraisal template", id: id.to_string() }.into_graphql())?)
            }
            None => None,
        };
        if existing.as_ref().is_some_and(|row| row.status != "DRAFT" || row.performance_program_id != program_id) {
            return Err(KabiPayError::Validation(
                "Only a draft template in the same performance program can be edited".into(),
            ).into_graphql());
        }
        let template_id = existing.as_ref().map(|row| row.id).unwrap_or_else(Uuid::new_v4);
        let version = if let Some(row) = &existing {
            row.version
        } else {
            let row = txn.query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                "SELECT COALESCE(MAX(version), 0) + 1 AS next_version FROM appraisal_template WHERE tenant_id = $1 AND performance_program_id = $2",
                [tenant_id.into(), program_id.into()],
            )).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
                .expect("aggregate query always returns a row");
            row.try_get::<i32>("", "next_version")
                .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
        };
        if let Some(row) = existing {
            let mut model = row.into_active_model();
            model.name = Set(name.clone());
            model.updated_at = Set(chrono::Utc::now());
            model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            appraisal_template_section::Entity::delete_many()
                .filter(appraisal_template_section::Column::TenantId.eq(tenant_id))
                .filter(appraisal_template_section::Column::AppraisalTemplateId.eq(template_id))
                .exec(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        } else {
            appraisal_template::ActiveModel {
                id: Set(template_id), tenant_id: Set(tenant_id),
                performance_program_id: Set(program_id), version: Set(version), name: Set(name),
                status: Set("DRAFT".into()), published_at: Set(None), published_by: Set(None),
                created_at: Set(chrono::Utc::now()), updated_at: Set(chrono::Utc::now()),
            }.insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        }
        for (section_index, section) in input.sections.iter().enumerate() {
            let section_id = Uuid::new_v4();
            appraisal_template_section::ActiveModel {
                id: Set(section_id), tenant_id: Set(tenant_id), appraisal_template_id: Set(template_id),
                title: Set(section.title.trim().to_owned()),
                description: Set(section.description.as_ref().map(|value| value.trim().to_owned()).filter(|value| !value.is_empty())),
                display_order: Set(section_index as i32), created_at: Set(chrono::Utc::now()),
            }.insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            let ids: HashMap<String, Uuid> = section.questions.iter()
                .map(|question| (question.client_key.trim().to_owned(), Uuid::new_v4())).collect();
            for parent_pass in [true, false] {
                for (question_index, question) in section.questions.iter().enumerate() {
                    if question.parent_client_key.is_none() != parent_pass { continue; }
                    let question_id = ids[question.client_key.trim()];
                    let parent_id = question.parent_client_key.as_ref().map(|key| ids[key.trim()]);
                    appraisal_question::ActiveModel {
                        id: Set(question_id), tenant_id: Set(tenant_id), section_id: Set(section_id),
                        parent_question_id: Set(parent_id),
                        question_type: Set(question.question_type.trim().to_ascii_uppercase()),
                        prompt: Set(question.prompt.trim().to_owned()), is_required: Set(question.is_required),
                        answerer: Set(question.answerer.trim().to_ascii_uppercase()),
                        self_rating_enabled: Set(question.self_rating_enabled),
                        manager_rating_enabled: Set(question.manager_rating_enabled),
                        display_order: Set(question_index as i32), created_at: Set(chrono::Utc::now()),
                    }.insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
                    for (option_index, option) in question.options.iter().enumerate() {
                        appraisal_question_option::ActiveModel {
                            id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), question_id: Set(question_id),
                            label: Set(option.label.trim().to_owned()),
                            score: Set(option.score.as_deref().map(|score| score.trim().parse()).transpose()
                                .map_err(|_| KabiPayError::Validation("Option score must be a valid number".into()).into_graphql())?),
                            display_order: Set(option_index as i32), created_at: Set(chrono::Utc::now()),
                        }.insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
                    }
                }
            }
        }
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_workflow::load_template(&db, tenant_id, template_id).await.map_err(KabiPayError::into_graphql)
    }

    async fn publish_appraisal_template(
        &self,
        ctx: &Context<'_>,
        appraisal_template_id: ID,
    ) -> Result<AppraisalTemplateDto> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let template_id = parse_id(&appraisal_template_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let template = appraisal_template::Entity::find_by_id(template_id)
            .filter(appraisal_template::Column::TenantId.eq(tenant_id))
            .lock_exclusive()
            .one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "appraisal template", id: template_id.to_string() }.into_graphql())?;
        if template.status != "DRAFT" {
            return Err(KabiPayError::Validation("Only draft appraisal templates can be published".into()).into_graphql());
        }
        let loaded = performance_workflow::load_template(&txn, tenant_id, template_id).await.map_err(KabiPayError::into_graphql)?;
        if loaded.sections.is_empty() || loaded.sections.iter().all(|section| section.questions.is_empty()) {
            return Err(KabiPayError::Validation("A template requires questions before publication".into()).into_graphql());
        }
        let mut model = template.into_active_model();
        model.status = Set("PUBLISHED".into());
        model.published_at = Set(Some(chrono::Utc::now()));
        model.published_by = Set(Some(claims.sub));
        model.updated_at = Set(chrono::Utc::now());
        model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let published = performance_workflow::load_template(&txn, tenant_id, template_id).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(published)
    }

    async fn activate_performance_program(
        &self,
        ctx: &Context<'_>,
        performance_program_id: ID,
    ) -> Result<PerformanceProgramDto> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let program_id = parse_id(&performance_program_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let published_count = appraisal_template::Entity::find()
            .filter(appraisal_template::Column::TenantId.eq(tenant_id))
            .filter(appraisal_template::Column::PerformanceProgramId.eq(program_id))
            .filter(appraisal_template::Column::Status.eq("PUBLISHED"))
            .count(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if published_count == 0 {
            return Err(KabiPayError::Validation("Publish an appraisal template before activating the program".into()).into_graphql());
        }
        let row = performance_program::Entity::find_by_id(program_id)
            .filter(performance_program::Column::TenantId.eq(tenant_id))
            .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance program", id: program_id.to_string() }.into_graphql())?;
        if row.status == "ARCHIVED" {
            return Err(KabiPayError::Validation("Archived performance programs cannot be activated".into()).into_graphql());
        }
        let mut model = row.into_active_model();
        model.status = Set("ACTIVE".into());
        model.updated_at = Set(chrono::Utc::now());
        model.update(&db).await.map(Into::into).map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)
    }

    async fn launch_performance_cycle(
        &self,
        ctx: &Context<'_>,
        input: LaunchPerformanceCycleInput,
    ) -> Result<ReviewCycleDto> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let program_id = parse_id(&input.performance_program_id)?;
        let template_id = parse_id(&input.appraisal_template_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let program = performance_program::Entity::find_by_id(program_id)
            .filter(performance_program::Column::TenantId.eq(tenant_id))
            .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance program", id: program_id.to_string() }.into_graphql())?;
        if program.status != "ACTIVE" {
            return Err(KabiPayError::Validation("Activate the performance program before launching a cycle".into()).into_graphql());
        }
        let template = appraisal_template::Entity::find_by_id(template_id)
            .filter(appraisal_template::Column::TenantId.eq(tenant_id))
            .filter(appraisal_template::Column::PerformanceProgramId.eq(program_id))
            .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "appraisal template", id: template_id.to_string() }.into_graphql())?;
        if template.status != "PUBLISHED" {
            return Err(KabiPayError::Validation("Only a published appraisal template can be launched".into()).into_graphql());
        }
        let cadence = match program.cadence.as_str() {
            "MONTHLY" => performance_lifecycle::Cadence::Monthly,
            "QUARTERLY" => performance_lifecycle::Cadence::Quarterly,
            "YEARLY" => performance_lifecycle::Cadence::Yearly,
            "MANUAL" => performance_lifecycle::Cadence::Manual,
            _ => return Err(KabiPayError::Validation("Performance program cadence is invalid".into()).into_graphql()),
        };
        let period = performance_lifecycle::period_for_date(cadence, input.period_date);
        if let Some(self_due) = input.self_review_due_date {
            if self_due < period.start_date || self_due > period.end_date {
                return Err(KabiPayError::Validation("Self-review due date must fall inside the cycle period".into()).into_graphql());
            }
        }
        if let (Some(self_due), Some(manager_due)) = (input.self_review_due_date, input.manager_review_due_date) {
            if manager_due < self_due {
                return Err(KabiPayError::Validation("Manager-review due date cannot precede the self-review due date".into()).into_graphql());
            }
        }
        let cycle_id = Uuid::new_v4();
        let now = chrono::Utc::now();
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let policy = performance_policy::lock_launch_policy(&txn, tenant_id, program_id)
            .await
            .map_err(KabiPayError::into_graphql)?;
        if policy.program_status != "ACTIVE" {
            return Err(KabiPayError::Validation("Archived performance programs cannot launch cycles".into()).into_graphql());
        }
        let deadlines = performance_policy::snapshot_cycle_deadlines(
            &policy,
            period.start_date,
            performance_policy::ManualDeadlineOverrides {
                self_review_due_date: input.self_review_due_date,
                manager_review_due_date: input.manager_review_due_date,
            },
        )
        .map_err(KabiPayError::into_graphql)?;
        let insert = txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            r#"INSERT INTO review_cycle
               (id, tenant_id, name, start_date, end_date, status, review_type, created_at, updated_at,
                performance_program_id, period_key, appraisal_template_id, current_stage,
                goal_setting_due_date, self_review_due_date, manager_review_due_date, calibration_due_date,
                acknowledgement_due_date, launched_at)
               VALUES ($1,$2,$3,$4,$5,'ACTIVE',$6,$7,$7,$8,$9,$10,'GOAL_SETTING',$11,$12,$13,$14,$15,$7)"#,
            vec![cycle_id.into(), tenant_id.into(), format!("{} {}", program.name, period.key).into(),
                period.start_date.into(), period.end_date.into(), program.cadence.clone().into(), now.into(),
                program_id.into(), period.key.into(), template_id.into(), deadlines.goal_setting_due_date.into(),
                deadlines.self_review_due_date.into(), deadlines.manager_review_due_date.into(),
                deadlines.calibration_due_date.into(), deadlines.acknowledgement_due_date.into()],
        )).await;
        if let Err(error) = insert {
            let message = error.to_string();
            if message.contains("uq_review_cycle_program_period") {
                return Err(KabiPayError::Conflict("This program period has already been launched".into()).into_graphql());
            }
            return Err(KabiPayError::from(error).into_graphql());
        }
        let participants = performance_policy::insert_eligible_participants(
            &txn, tenant_id, program_id, cycle_id, template_id, now, &policy,
        )
        .await
        .map_err(KabiPayError::into_graphql)?;
        if participants == 0 {
            return Err(KabiPayError::Validation("No active employee-linked accounts matched this cycle's launch policy".into()).into_graphql());
        }
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        review_cycle::Entity::find_by_id(cycle_id).filter(review_cycle::Column::TenantId.eq(tenant_id))
            .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .map(Into::into).ok_or_else(|| KabiPayError::NotFound { entity: "review cycle", id: cycle_id.to_string() }.into_graphql())
    }

    async fn propose_performance_goal(
        &self,
        ctx: &Context<'_>,
        input: SavePerformanceGoalInput,
    ) -> Result<GoalDto> {
        let claims = require_client_claims(ctx)?;
        require_goal_mutation_authority(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&input.participant_id)?;
        let title = validate_text(&input.title, 255)?;
        let description = optional_text(input.description, 4000)?;
        let weight = parse_decimal(&input.weightage, "Goal weight")?;
        if weight <= Decimal::ZERO || weight > Decimal::ONE_HUNDRED {
            return Err(KabiPayError::Validation("Goal weight must be greater than 0 and at most 100".into()).into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        require_goal_setting(&participant, &stage)?;
        goal_actor_can_manage(claims, &participant, false, None)?;
        let now = chrono::Utc::now();
        let saved = goal::ActiveModel {
            id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), employee_id: Set(participant.employee_id),
            review_cycle_id: Set(participant.review_cycle_id), parent_goal_id: Set(None), title: Set(title),
            description: Set(description), weightage: Set(Some(weight)), status: Set("PROPOSED".into()),
            visibility: Set(Some("EMPLOYEE_MANAGER".into())), created_at: Set(now), updated_at: Set(now),
        }.insert(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
    }

    async fn update_performance_goal(
        &self,
        ctx: &Context<'_>,
        goal_id: ID,
        input: SavePerformanceGoalInput,
    ) -> Result<GoalDto> {
        let claims = require_client_claims(ctx)?;
        require_goal_mutation_authority(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&input.participant_id)?;
        let goal_id = parse_id(&goal_id)?;
        let title = validate_text(&input.title, 255)?;
        let description = optional_text(input.description, 4000)?;
        let weight = parse_decimal(&input.weightage, "Goal weight")?;
        if weight <= Decimal::ZERO || weight > Decimal::ONE_HUNDRED {
            return Err(KabiPayError::Validation("Goal weight must be greater than 0 and at most 100".into()).into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        require_goal_setting(&participant, &stage)?;
        let existing = goal::Entity::find_by_id(goal_id)
            .filter(goal::Column::TenantId.eq(tenant_id))
            .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
            .filter(goal::Column::EmployeeId.eq(participant.employee_id))
            .lock_exclusive().one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance goal", id: goal_id.to_string() }.into_graphql())?;
        goal_actor_can_manage(claims, &participant, true, Some(existing.status.as_str()))?;
        let mut model = existing.into_active_model();
        model.title = Set(title); model.description = Set(description); model.weightage = Set(Some(weight));
        model.status = Set("PROPOSED".into()); model.updated_at = Set(chrono::Utc::now());
        let saved = model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
    }

    async fn delete_performance_goal(&self, ctx: &Context<'_>, participant_id: ID, goal_id: ID) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        require_goal_mutation_authority(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let goal_id = parse_id(&goal_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        require_goal_setting(&participant, &stage)?;
        let existing = goal::Entity::find_by_id(goal_id).filter(goal::Column::TenantId.eq(tenant_id))
            .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id)).filter(goal::Column::EmployeeId.eq(participant.employee_id))
            .lock_exclusive().one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance goal", id: goal_id.to_string() }.into_graphql())?;
        goal_actor_can_manage(claims, &participant, true, Some(existing.status.as_str()))?;
        let kpis = kpi::Entity::find().filter(kpi::Column::TenantId.eq(tenant_id)).filter(kpi::Column::GoalId.eq(goal_id)).count(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let children = goal::Entity::find().filter(goal::Column::TenantId.eq(tenant_id)).filter(goal::Column::ParentGoalId.eq(goal_id)).count(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let feedback = continuous_feedback::Entity::find().filter(continuous_feedback::Column::TenantId.eq(tenant_id)).filter(continuous_feedback::Column::GoalId.eq(goal_id)).count(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if kpis > 0 || children > 0 || feedback > 0 {
            return Err(KabiPayError::Conflict("This goal has dependent KPI, child goal, or feedback records".into()).into_graphql());
        }
        existing.delete(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        goal::Entity::update_many().col_expr(goal::Column::Status, Expr::value("PROPOSED"))
            .col_expr(goal::Column::UpdatedAt, Expr::value(chrono::Utc::now()))
            .filter(goal::Column::TenantId.eq(tenant_id)).filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
            .filter(goal::Column::EmployeeId.eq(participant.employee_id)).exec(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }

    async fn approve_performance_goals(
        &self,
        ctx: &Context<'_>,
        participant_id: ID,
    ) -> Result<Vec<GoalDto>> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            require_employee_id(claims)?;
        }
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        require_goal_setting(&participant, &stage)?;
        if !claims.can_manage_performance_programs()
            && (!claims.can_evaluate_performance_team() || !claims.employee_id.is_some_and(|actor| performance_lifecycle::manager_matches_snapshot(actor, participant.manager_employee_id))) {
            return Err(KabiPayError::Forbidden("Only the review's assigned manager can approve its goals".into()).into_graphql());
        }
        let goals = goal::Entity::find()
            .filter(goal::Column::TenantId.eq(tenant_id))
            .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
            .filter(goal::Column::EmployeeId.eq(participant.employee_id))
            .all(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if goals.iter().any(|goal| goal.weightage.is_none()) {
            return Err(KabiPayError::Validation("Every goal must have a weight before approval".into()).into_graphql());
        }
        let weights: Vec<Decimal> = goals.iter().filter_map(|goal| goal.weightage).collect();
        performance_lifecycle::validate_goal_weight_total(&weights)
            .map_err(|message| KabiPayError::Validation(message).into_graphql())?;
        for goal in goals {
            let mut model = goal.into_active_model();
            model.status = Set("APPROVED".into());
            model.updated_at = Set(chrono::Utc::now());
            model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        }
        let approved = goal::Entity::find()
            .filter(goal::Column::TenantId.eq(tenant_id))
            .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
            .filter(goal::Column::EmployeeId.eq(participant.employee_id))
            .order_by_asc(goal::Column::CreatedAt)
            .all(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(approved.into_iter().map(Into::into).collect())
    }

    async fn add_performance_feedback(
        &self,
        ctx: &Context<'_>,
        input: AddPerformanceFeedbackInput,
    ) -> Result<PerformanceFeedbackDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            require_employee_id(claims)?;
        }
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&input.participant_id)?;
        let comments = validate_text(&input.comments, 4000)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let participant = performance_workflow::load_participant(&db, tenant_id, participant_id).await.map_err(KabiPayError::into_graphql)?;
        if !claims.can_manage_performance_programs()
            && (!claims.can_evaluate_performance_team()
                || !claims.employee_id.is_some_and(|actor| {
                    performance_lifecycle::manager_matches_snapshot(actor, participant.manager_employee_id)
                }))
        {
            return Err(KabiPayError::Forbidden("Only the review's assigned manager can provide feedback".into()).into_graphql());
        }
        let goal_id = input.goal_id.as_ref().map(parse_id).transpose()?;
        if let Some(goal_id) = goal_id {
            let is_review_goal = goal::Entity::find_by_id(goal_id)
                .filter(goal::Column::TenantId.eq(tenant_id))
                .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
                .filter(goal::Column::EmployeeId.eq(participant.employee_id))
                .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
                .is_some();
            if !is_review_goal {
                return Err(KabiPayError::Validation("Feedback goal must belong to this employee review".into()).into_graphql());
            }
        }
        continuous_feedback::ActiveModel {
            id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), review_cycle_id: Set(Some(participant.review_cycle_id)),
            goal_id: Set(goal_id), reviewer_employee_id: Set(claims.employee_id), reviewee_employee_id: Set(participant.employee_id),
            visibility: Set("EMPLOYEE_VISIBLE".into()), observation_date: Set(input.observation_date),
            comments: Set(comments), created_by_user_id: Set(claims.sub), created_at: Set(chrono::Utc::now()),
        }.insert(&db).await.map(Into::into).map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)
    }

    async fn advance_performance_cycle(
        &self,
        ctx: &Context<'_>,
        review_cycle_id: ID,
    ) -> Result<String> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let cycle_id = parse_id(&review_cycle_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let next = performance_administration::advance_cycle_with_context(&txn, tenant_id, cycle_id, "", Some(claims.sub), "MANUAL").await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if next == "BLOCKED" {
            return Err(KabiPayError::Validation("Performance cycle has unresolved prerequisites; review actionable exceptions".into()).into_graphql());
        }
        Ok(next)
    }

    async fn submit_self_appraisal(
        &self,
        ctx: &Context<'_>,
        participant_id: ID,
        answers: Vec<AppraisalAnswerInput>,
        expected_revision: Option<i32>,
    ) -> Result<PerformanceReviewDetailDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_use_performance_self_service() {
            return Err(KabiPayError::Forbidden("performance:self with SELF scope required".into()).into_graphql());
        }
        let employee_id = require_employee_id(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let cycle_id = performance_participant::Entity::find_by_id(participant_id).filter(performance_participant::Column::TenantId.eq(tenant_id)).one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?.review_cycle_id;
        let stage = performance_workflow::locked_cycle_stage(&txn, tenant_id, cycle_id).await.map_err(KabiPayError::into_graphql)?;
        let participant = performance_participant::Entity::find_by_id(participant_id)
            .filter(performance_participant::Column::TenantId.eq(tenant_id))
            .lock_exclusive()
            .one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?;
        require_appraisal_submission_state(&participant, cycle_id, false)?;
        require_expected_revision(participant.response_revision, expected_revision)?;
        if participant.employee_id != employee_id {
            return Err(KabiPayError::Forbidden("This self-appraisal belongs to another employee".into()).into_graphql());
        }
        if participant.self_submitted_at.is_some() {
            return Err(KabiPayError::Conflict("This self-appraisal has already been submitted".into()).into_graphql());
        }
        if stage != "SELF_REVIEW" {
            return Err(KabiPayError::Validation("Self-appraisal is only available during the self-review stage".into()).into_graphql());
        }
        save_appraisal_answers(&txn, tenant_id, &participant, answers, AnswerRole::Employee).await?;
        let mut model = participant.into_active_model();
        model.self_submitted_at = Set(Some(chrono::Utc::now()));
        model.status = Set("SELF_SUBMITTED".into());
        let participant = model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::snapshot_revision(&txn, tenant_id, participant.id).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_workflow::load_review_detail(&db, tenant_id, participant).await.map_err(KabiPayError::into_graphql)
    }

    async fn submit_manager_appraisal(
        &self,
        ctx: &Context<'_>,
        participant_id: ID,
        answers: Vec<AppraisalAnswerInput>,
        final_rating: String,
        performance_band: Option<String>,
        expected_revision: Option<i32>,
    ) -> Result<PerformanceReviewDetailDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_performance_programs() {
            require_employee_id(claims)?;
        }
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let final_rating = parse_decimal(&final_rating, "Final rating")?;
        let performance_band = optional_text(performance_band, 50)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let cycle_id = performance_participant::Entity::find_by_id(participant_id).filter(performance_participant::Column::TenantId.eq(tenant_id)).one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?.review_cycle_id;
        let stage = performance_workflow::locked_cycle_stage(&txn, tenant_id, cycle_id).await.map_err(KabiPayError::into_graphql)?;
        let participant = performance_participant::Entity::find_by_id(participant_id)
            .filter(performance_participant::Column::TenantId.eq(tenant_id))
            .lock_exclusive()
            .one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?;
        require_appraisal_submission_state(&participant, cycle_id, true)?;
        require_expected_revision(participant.response_revision, expected_revision)?;
        if !claims.can_manage_performance_programs()
            && (!claims.can_evaluate_performance_team() || !claims.employee_id.is_some_and(|actor| {
                performance_lifecycle::manager_matches_snapshot(actor, participant.manager_employee_id)
            }))
        {
            return Err(KabiPayError::Forbidden("Only the review's assigned manager can submit this appraisal".into()).into_graphql());
        }
        if participant.manager_submitted_at.is_some() {
            return Err(KabiPayError::Conflict("This manager appraisal has already been submitted".into()).into_graphql());
        }
        if stage != "MANAGER_REVIEW" {
            return Err(KabiPayError::Validation("Manager appraisal is only available during the manager-review stage".into()).into_graphql());
        }
        let program = performance_workflow::program_for_cycle(&txn, tenant_id, participant.review_cycle_id).await.map_err(KabiPayError::into_graphql)?;
        performance_lifecycle::validate_rating(final_rating, program.rating_min, program.rating_max)
            .map_err(|message| KabiPayError::Validation(message).into_graphql())?;
        save_appraisal_answers(&txn, tenant_id, &participant, answers, AnswerRole::Manager).await?;
        let mut model = participant.into_active_model();
        model.manager_submitted_at = Set(Some(chrono::Utc::now()));
        model.manager_rating = Set(Some(final_rating));
        model.manager_performance_band = Set(performance_band.clone());
        model.final_rating = Set((!program.include_calibration).then_some(final_rating));
        model.performance_band = Set((!program.include_calibration).then_some(performance_band).flatten());
        model.status = Set("MANAGER_SUBMITTED".into());
        let participant = model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::snapshot_revision(&txn, tenant_id, participant.id).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_workflow::load_review_detail(&db, tenant_id, participant).await.map_err(KabiPayError::into_graphql)
    }

    async fn acknowledge_performance_review(
        &self,
        ctx: &Context<'_>,
        participant_id: ID,
        comment: Option<String>,
        expected_revision: Option<i32>,
    ) -> Result<PerformanceReviewDetailDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_use_performance_self_service() {
            return Err(KabiPayError::Forbidden("performance:self with SELF scope required".into()).into_graphql());
        }
        let employee_id = require_employee_id(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let comment = optional_text(comment, 2000)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        require_expected_revision(participant.response_revision, expected_revision)?;
        if participant.employee_id != employee_id {
            return Err(KabiPayError::Forbidden("This review belongs to another employee".into()).into_graphql());
        }
        if participant.is_excluded {
            return Err(KabiPayError::Validation("Excluded participants cannot acknowledge a review".into()).into_graphql());
        }
        if stage != "EMPLOYEE_ACKNOWLEDGEMENT" {
            return Err(KabiPayError::Validation("This review is not awaiting employee acknowledgement".into()).into_graphql());
        }
        if participant.acknowledged_at.is_some() {
            txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
            return performance_workflow::load_review_detail(&db, tenant_id, participant).await.map_err(KabiPayError::into_graphql);
        }
        let mut model = participant.into_active_model();
        model.acknowledged_at = Set(Some(chrono::Utc::now()));
        model.acknowledgement_comment = Set(comment);
        model.status = Set("ACKNOWLEDGED".into());
        model.updated_at = Set(chrono::Utc::now());
        let participant = model.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::snapshot_revision(&txn, tenant_id, participant.id).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_workflow::load_review_detail(&db, tenant_id, participant).await.map_err(KabiPayError::into_graphql)
    }

}

#[derive(Clone, Copy, PartialEq, Eq)]
enum AnswerRole {
    Employee,
    Manager,
}

async fn save_appraisal_answers<C>(
    db: &C,
    tenant_id: Uuid,
    participant: &performance_participant::Model,
    answers: Vec<AppraisalAnswerInput>,
    role: AnswerRole,
) -> Result<()>
where
    C: ConnectionTrait,
{
    let sections = appraisal_template_section::Entity::find()
        .filter(appraisal_template_section::Column::TenantId.eq(tenant_id))
        .filter(appraisal_template_section::Column::AppraisalTemplateId.eq(participant.appraisal_template_id))
        .all(db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    let section_ids: Vec<Uuid> = sections.into_iter().map(|section| section.id).collect();
    let questions = if section_ids.is_empty() { Vec::new() } else {
        appraisal_question::Entity::find()
            .filter(appraisal_question::Column::TenantId.eq(tenant_id))
            .filter(appraisal_question::Column::SectionId.is_in(section_ids))
            .all(db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
    };
    let eligible: HashMap<Uuid, appraisal_question::Model> = questions.into_iter()
        .filter(|question| match role {
            AnswerRole::Employee => question.answerer == "EMPLOYEE" || question.answerer == "BOTH",
            AnswerRole::Manager => question.answerer == "MANAGER" || question.answerer == "BOTH",
        })
        .map(|question| (question.id, question)).collect();
    if eligible.is_empty() {
        return Err(KabiPayError::Validation("The appraisal template has no questions for this reviewer".into()).into_graphql());
    }
    let mut response_ids = HashSet::new();
    let mut parsed = Vec::with_capacity(answers.len());
    for answer in answers {
        let question_id = parse_id(&answer.question_id)?;
        if !response_ids.insert(question_id) {
            return Err(KabiPayError::Validation("Each appraisal question can be answered only once".into()).into_graphql());
        }
        let question = eligible.get(&question_id).ok_or_else(|| {
            KabiPayError::Validation("An answer references a question outside this appraisal or reviewer role".into()).into_graphql()
        })?;
        let text = optional_text(answer.employee_text_answer_alias(), 8000)?;
        let option_ids: Vec<Uuid> = answer.employee_option_ids_alias().iter().map(parse_id).collect::<Result<_>>()?;
        let rating = answer.employee_rating_alias().as_deref().map(|value| parse_decimal(value, "Rating")).transpose()?;
        let is_choice = question.question_type == "SINGLE_CHOICE" || question.question_type == "MULTIPLE_CHOICE";
        if is_choice && text.is_some() {
            return Err(KabiPayError::Validation("Choice questions cannot contain a text answer".into()).into_graphql());
        }
        if !is_choice && !option_ids.is_empty() {
            return Err(KabiPayError::Validation("Only choice questions accept selected options".into()).into_graphql());
        }
        if question.question_type == "SINGLE_CHOICE" && option_ids.len() > 1 {
            return Err(KabiPayError::Validation("Single-choice questions accept one option".into()).into_graphql());
        }
        let rating_enabled = question.question_type == "RATING" || match role {
            AnswerRole::Employee => question.self_rating_enabled,
            AnswerRole::Manager => question.manager_rating_enabled,
        };
        if rating.is_some() && !rating_enabled {
            return Err(KabiPayError::Validation("Rating is not enabled for this question and reviewer".into()).into_graphql());
        }
        let has_primary_answer = match question.question_type.as_str() {
            "SHORT_TEXT" | "LONG_TEXT" => text.is_some(),
            "SINGLE_CHOICE" | "MULTIPLE_CHOICE" => !option_ids.is_empty(),
            "RATING" => rating.is_some(),
            _ => false,
        };
        if question.is_required && !has_primary_answer {
            return Err(KabiPayError::Validation(format!("Required question '{}' must be answered", question.prompt)).into_graphql());
        }
        parsed.push((question_id, text, option_ids, rating));
    }
    for question in eligible.values().filter(|question| question.is_required) {
        if !response_ids.contains(&question.id) {
            return Err(KabiPayError::Validation(format!("Required question '{}' must be answered", question.prompt)).into_graphql());
        }
    }
    let selected_ids: Vec<Uuid> = parsed.iter().flat_map(|(_, _, ids, _)| ids.iter().copied()).collect();
    if !selected_ids.is_empty() {
        let matching = appraisal_question_option::Entity::find()
            .filter(appraisal_question_option::Column::TenantId.eq(tenant_id))
            .filter(appraisal_question_option::Column::Id.is_in(selected_ids.clone()))
            .all(db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if matching.len() != selected_ids.len()
            || matching.iter().any(|option| !parsed.iter().any(|(question_id, _, ids, _)| *question_id == option.question_id && ids.contains(&option.id)))
        {
            return Err(KabiPayError::Validation("A selected option does not belong to its appraisal question".into()).into_graphql());
        }
    }
    let program = performance_workflow::program_for_cycle(db, tenant_id, participant.review_cycle_id)
        .await.map_err(KabiPayError::into_graphql)?;
    for (_, _, _, rating) in &parsed {
        if let Some(rating) = rating {
            performance_lifecycle::validate_rating(*rating, program.rating_min, program.rating_max)
                .map_err(|message| KabiPayError::Validation(message).into_graphql())?;
        }
    }
    for (question_id, text, option_ids, rating) in parsed {
        let existing = appraisal_answer::Entity::find()
            .filter(appraisal_answer::Column::TenantId.eq(tenant_id))
            .filter(appraisal_answer::Column::PerformanceParticipantId.eq(participant.id))
            .filter(appraisal_answer::Column::QuestionId.eq(question_id))
            .filter(appraisal_answer::Column::Revision.eq(participant.response_revision))
            .one(db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let is_new = existing.is_none();
        let mut model = existing.map(IntoActiveModel::into_active_model).unwrap_or_else(|| appraisal_answer::ActiveModel {
            id: Set(Uuid::new_v4()), tenant_id: Set(tenant_id), performance_participant_id: Set(participant.id),
            question_id: Set(question_id), revision: Set(participant.response_revision),
            employee_text_answer: Set(None), employee_selected_option_ids: Set(None), self_rating: Set(None),
            manager_text_answer: Set(None), manager_selected_option_ids: Set(None), manager_rating: Set(None),
            created_at: Set(chrono::Utc::now()), updated_at: Set(chrono::Utc::now()),
        });
        let option_json = (!option_ids.is_empty()).then(|| serde_json::Value::Array(option_ids.iter().map(|id| serde_json::Value::String(id.to_string())).collect()));
        match role {
            AnswerRole::Employee => {
                model.employee_text_answer = Set(text);
                model.employee_selected_option_ids = Set(option_json);
                model.self_rating = Set(rating);
            }
            AnswerRole::Manager => {
                model.manager_text_answer = Set(text);
                model.manager_selected_option_ids = Set(option_json);
                model.manager_rating = Set(rating);
            }
        }
        model.updated_at = Set(chrono::Utc::now());
        if is_new { model.insert(db).await } else { model.update(db).await }
            .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    }
    Ok(())
}

impl AppraisalAnswerInput {
    fn employee_text_answer_alias(&self) -> Option<String> { self.text_answer.clone() }
    fn employee_option_ids_alias(&self) -> &[ID] { &self.selected_option_ids }
    fn employee_rating_alias(&self) -> Option<String> { self.rating.clone() }
}

#[cfg(test)]
#[path = "answer_validation_tests.rs"]
mod answer_validation_tests;

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn unlinked_team_evaluator_cannot_access_participant_workflows() {
        let claims: kabipay_common::context::ClientClaims =
            serde_json::from_value(serde_json::json!({
                "sub": Uuid::new_v4(), "tenant_id": Uuid::new_v4(),
                "iss": "kabipay-client", "iat": 0, "exp": 9999999999i64,
                "permissions": ["performance:evaluate"],
                "permission_scopes": {"performance:evaluate": "TEAM"}
            })).unwrap();
        let schema = async_graphql::Schema::build(
            crate::resolvers::QueryRoot::default(), crate::resolvers::MutationRoot::default(), async_graphql::EmptySubscription,
        ).data(kabipay_common::subgraph::TenantId(claims.tenant_id)).data(claims).finish();
        let id = Uuid::new_v4();
        let operations = [
            format!(r#"{{ performanceReviewDetail(participantId: "{id}") {{ review {{ id }} }} }}"#),
            format!(r#"mutation {{ approvePerformanceGoals(participantId: "{id}") {{ id }} }}"#),
            format!(r#"mutation {{ addPerformanceFeedback(input: {{ participantId: "{id}", comments: "Feedback", observationDate: "2026-09-10" }}) {{ id }} }}"#),
            format!(r#"mutation {{ submitManagerAppraisal(participantId: "{id}", answers: [], finalRating: "3") {{ review {{ id }} }} }}"#),
        ];
        for operation in operations {
            let response = schema.execute(&operation).await;
            assert_eq!(response.errors.len(), 1, "{operation}: {:?}", response.errors);
            assert!(response.errors[0].message.contains("employee-linked"),
                "{operation}: {:?}", response.errors);
        }
    }

    #[test]
    fn rejects_blank_and_overlong_names() {
        assert!(validate_text("  ", 255).is_err());
        assert!(validate_text(&"a".repeat(256), 255).is_err());
        assert_eq!(validate_text(" Skills ", 255).unwrap(), "Skills");
    }

    #[test]
    fn goal_mutation_authority_requires_a_concrete_identity_unless_manage_all() {
        let mut claims: kabipay_common::context::ClientClaims = serde_json::from_value(serde_json::json!({
            "sub": Uuid::new_v4(), "tenant_id": Uuid::new_v4(), "iss": "kabipay-client",
            "iat": 0, "exp": 9999999999i64, "permissions": ["performance:evaluate"],
            "permission_scopes": {"performance:evaluate": "TEAM"}
        })).unwrap();
        assert!(require_goal_mutation_authority(&claims).is_err());
        claims.employee_id = Some(Uuid::new_v4());
        assert!(require_goal_mutation_authority(&claims).is_ok());
        claims.employee_id = None;
        claims.permissions = vec!["performance:manage".into()];
        claims.permission_scopes = std::collections::HashMap::from([("performance:manage".into(), "ALL".into())]);
        assert!(require_goal_mutation_authority(&claims).is_ok());
    }

    #[test]
    fn excluded_or_stale_goal_participants_are_rejected_before_writes() {
        let participant = performance_participant::Model {
            id: Uuid::new_v4(), tenant_id: Uuid::new_v4(), review_cycle_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(), manager_employee_id: None, department_id: None,
            designation_id: None, work_location_id: None, appraisal_template_id: Uuid::new_v4(),
            status: "GOAL_SETTING".into(), is_excluded: true, exclusion_reason: Some("leave".into()),
            response_revision: 1, self_submitted_at: None, manager_submitted_at: None,
            acknowledged_at: None, acknowledgement_comment: None, final_rating: None,
            performance_band: None, manager_rating: None, manager_performance_band: None, calibration_provenance: None, created_at: chrono::Utc::now(), updated_at: chrono::Utc::now(),
        };
        assert!(require_goal_setting(&participant, "GOAL_SETTING").is_err());
        let mut active = participant;
        active.is_excluded = false;
        assert!(require_goal_setting(&active, "SELF_REVIEW").is_err());
        assert!(require_goal_setting(&active, "GOAL_SETTING").is_ok());
        active.is_excluded = true;
        assert!(require_appraisal_submission_state(&active, active.review_cycle_id, false).is_err());
        active.is_excluded = false;
        assert!(require_appraisal_submission_state(&active, Uuid::new_v4(), false).is_err());
        assert!(require_appraisal_submission_state(&active, active.review_cycle_id, true).is_err());
    }

    #[test]
    fn revision_compatibility_accepts_only_initial_omission_or_matching_revision() {
        assert!(require_expected_revision(1, None).is_ok());
        assert!(require_expected_revision(2, None).is_err());
        assert!(require_expected_revision(2, Some(2)).is_ok());
        assert!(require_expected_revision(2, Some(1)).is_err());
    }

    #[tokio::test]
    async fn rejects_missing_or_narrow_manage_scope_before_database_access() {
        for scope in ["SELF", "TEAM", "DEPARTMENT", ""] {
            let claims: kabipay_common::context::ClientClaims =
                serde_json::from_value(serde_json::json!({
                    "sub": Uuid::new_v4(),
                    "tenant_id": Uuid::new_v4(),
                    "iss": "kabipay-client",
                    "iat": 0,
                    "exp": 9999999999i64,
                    "permissions": ["performance:manage"],
                    "permission_scopes": {"performance:manage": scope}
                })).unwrap();
            let schema = async_graphql::Schema::build(
                crate::resolvers::QueryRoot::default(),
                crate::resolvers::MutationRoot::default(),
                async_graphql::EmptySubscription,
            ).data(claims).finish();
            let response = schema.execute(
                r#"mutation { saveReviewCycle(input: {name: "Annual", startDate: "2026-01-01", endDate: "2026-12-31"}) { id } }"#,
            ).await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("ALL scope required"));
        }
    }
}
