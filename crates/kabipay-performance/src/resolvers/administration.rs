use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{subgraph::{require_client_claims, require_tenant_id}, KabiPayError};
#[cfg(not(test))]
use kabipay_common::subgraph::tenant_db;
#[cfg(test)]
use super::concurrency_tests::tenant_db;
use kabipay_db_entities::tenant::{d0018_performance::{goal, kpi}, d0075_performance_appraisal_lifecycle::{continuous_feedback, performance_participant, performance_program}};
use sea_orm::{ActiveModelTrait, ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait, IntoActiveModel, QueryFilter, QueryOrder, QuerySelect, Set, Statement, TransactionTrait, TryGetable};
use uuid::Uuid;

use super::mutation::{goal_actor_can_manage, locked_goal_context, optional_text, parse_decimal, parse_id, require_manage, validate_text};
use super::administration_history::load_admin_participant;
use super::administration_types::*;
use super::administration_pagination::{feedback_cursor as parse_feedback_cursor, page_limit};
use super::types::{PerformanceFeedbackDto, PerformanceProgramDto};
use crate::services::{performance_administration, performance_lifecycle, performance_workflow};


pub(super) fn kpi_dto(model: kpi::Model) -> PerformanceGoalKpiDto {
    PerformanceGoalKpiDto { id: model.id.to_string().into(), goal_id: model.goal_id.to_string().into(), metric_name: model.metric_name,
        target_value: model.target_value.map(|v| v.to_string()), actual_value: model.actual_value.map(|v| v.to_string()), unit: model.unit,
        evidence: model.evidence, comment: model.comment, measurement_date: model.measurement_date }
}

async fn load_kpi_for_participant(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    participant: &performance_participant::Model,
    kpi_id: Uuid,
) -> Result<kpi::Model> {
    let kpi = kpi::Entity::find_by_id(kpi_id)
        .filter(kpi::Column::TenantId.eq(tenant_id))
        .one(txn)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance KPI",
            id: kpi_id.to_string(),
        }.into_graphql())?;

    let belongs_to_participant = goal::Entity::find_by_id(kpi.goal_id)
        .filter(goal::Column::TenantId.eq(tenant_id))
        .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
        .filter(goal::Column::EmployeeId.eq(participant.employee_id))
        .one(txn)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .is_some();
    if !belongs_to_participant {
        return Err(KabiPayError::Forbidden(
            "KPI does not belong to this performance participant".into(),
        ).into_graphql());
    }

    Ok(kpi)
}

fn require_kpi_actual_authority(
    claims: &kabipay_common::context::ClientClaims,
    participant: &performance_participant::Model,
    stage: &str,
) -> Result<()> {
    if stage == "MANAGER_REVIEW" && claims.can_manage_performance_programs() {
        return Ok(());
    }
    let actor_employee_id = claims.employee_id.ok_or_else(|| {
        KabiPayError::Forbidden("An employee-linked account is required".into()).into_graphql()
    })?;

    match stage {
        "SELF_REVIEW"
            if claims.can_use_performance_self_service()
                && actor_employee_id == participant.employee_id => Ok(()),
        "MANAGER_REVIEW"
            if claims.can_evaluate_performance_team()
                && performance_lifecycle::manager_matches_snapshot(
                    actor_employee_id,
                    participant.manager_employee_id,
                ) => Ok(()),
        "SELF_REVIEW" | "MANAGER_REVIEW" => Err(KabiPayError::Forbidden(
            "You are not authorized to submit KPI actuals for this review stage".into(),
        ).into_graphql()),
        _ => Err(KabiPayError::Validation(
            "KPI actuals can only change during an open review stage".into(),
        ).into_graphql()),
    }
}

async fn locked_participant_for_administration(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> Result<(performance_participant::Model, String)> {
    let cycle_id = performance_participant::Entity::find_by_id(participant_id)
        .filter(performance_participant::Column::TenantId.eq(tenant_id))
        .one(txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?
        .review_cycle_id;
    let stage = performance_workflow::locked_cycle_stage(txn, tenant_id, cycle_id)
        .await.map_err(KabiPayError::into_graphql)?;
    let participant = performance_participant::Entity::find_by_id(participant_id)
        .filter(performance_participant::Column::TenantId.eq(tenant_id))
        .lock_exclusive().one(txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound { entity: "performance review", id: participant_id.to_string() }.into_graphql())?;
    if participant.review_cycle_id != cycle_id {
        return Err(KabiPayError::Conflict("Performance review changed while acquiring its cycle lock; retry the request".into()).into_graphql());
    }
    Ok((participant, stage))
}

pub struct AdministrationMutationRoot;

#[Object(name = "PerformanceAdministrationMutations")]
impl AdministrationMutationRoot {
    async fn save_performance_program_policy(
        &self,
        ctx: &Context<'_>,
        input: PerformanceProgramPolicyInput,
    ) -> Result<PerformanceProgramPolicyDto> {
        super::administration_policy::save_program_policy(ctx, input).await
    }
    async fn archive_performance_program(&self, ctx: &Context<'_>, performance_program_id: ID, reason: String) -> Result<PerformanceProgramDto> {
        let claims = require_client_claims(ctx)?; require_manage(claims)?; let tenant_id = require_tenant_id(ctx)?;
        let program_id = parse_id(&performance_program_id)?; let reason = validate_text(&reason, 4000)?; let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let program = performance_program::Entity::find_by_id(program_id).filter(performance_program::Column::TenantId.eq(tenant_id)).lock_exclusive().one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.ok_or_else(|| KabiPayError::NotFound { entity: "performance program", id: program_id.to_string() }.into_graphql())?;
        let now = chrono::Utc::now();
        txn.execute(Statement::from_sql_and_values(DatabaseBackend::Postgres, "INSERT INTO performance_program_policy (performance_program_id,tenant_id,population_mode,archived_at,archive_reason,updated_by,updated_at) VALUES ($1,$2,'ALL',$3,$4,$5,$3) ON CONFLICT (performance_program_id) DO UPDATE SET archived_at=EXCLUDED.archived_at,archive_reason=EXCLUDED.archive_reason,updated_by=EXCLUDED.updated_by,updated_at=EXCLUDED.updated_at", [program_id.into(),tenant_id.into(),now.into(),reason.clone().into(),claims.sub.into()])).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let mut updated = program.into_active_model(); updated.status = Set("ARCHIVED".into()); updated.updated_at = Set(now);
        let updated = updated.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::audit_program_event(&txn, tenant_id, program_id, "PROGRAM_ARCHIVED", &reason, claims.sub).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?; Ok(updated.into())
    }

    async fn save_performance_calibration(&self, ctx: &Context<'_>, input: SavePerformanceCalibrationInput) -> Result<PerformanceParticipantAdministrationDto> {
        let claims = require_client_claims(ctx)?; require_manage(claims)?; let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&input.participant_id)?; let rating = parse_decimal(&input.final_rating, "Final rating")?; let reason = validate_text(&input.reason, 4000)?; let band = optional_text(input.performance_band, 50)?;
        let db = tenant_db(ctx, tenant_id).await?; let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::set_calibration(&txn, tenant_id, participant_id, input.expected_revision, rating, band, &reason, claims.sub).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?; load_admin_participant(&db, tenant_id, participant_id).await
    }

    async fn reopen_performance_review(&self, ctx: &Context<'_>, input: ReopenPerformanceReviewInput) -> Result<PerformanceParticipantAdministrationDto> {
        let claims = require_client_claims(ctx)?; require_manage(claims)?; let tenant_id = require_tenant_id(ctx)?; let participant_id = parse_id(&input.participant_id)?;
        let reason = validate_text(&input.reason, 4000)?; let stage = input.correction_stage.trim().to_ascii_uppercase(); let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::reopen_revision(&txn, tenant_id, participant_id, input.expected_revision, &stage, &reason, claims.sub).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?; load_admin_participant(&db, tenant_id, participant_id).await
    }

    async fn set_performance_participant_excluded(&self, ctx: &Context<'_>, input: SetPerformanceParticipantExcludedInput) -> Result<PerformanceParticipantAdministrationDto> {
        let claims = require_client_claims(ctx)?; require_manage(claims)?; let tenant_id = require_tenant_id(ctx)?; let participant_id = parse_id(&input.participant_id)?; let reason = validate_text(&input.reason, 4000)?; let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_participant_for_administration(&txn, tenant_id, participant_id).await?;
        if stage != "GOAL_SETTING" { return Err(KabiPayError::Validation("Participants can only be excluded or restored before self review".into()).into_graphql()); }
        let cycle_id = participant.review_cycle_id;
        let mut updated = participant.into_active_model(); updated.is_excluded = Set(input.excluded); updated.exclusion_reason = Set(Some(reason.clone())); updated.status = Set(if input.excluded { "EXCLUDED".into() } else { "GOAL_SETTING".into() }); updated.updated_at = Set(chrono::Utc::now()); updated.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::audit_event(&txn, tenant_id, Some(cycle_id), Some(participant_id), if input.excluded { "PARTICIPANT_EXCLUDED" } else { "PARTICIPANT_RESTORED" }, Some(&reason), Some(claims.sub)).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?; load_admin_participant(&db, tenant_id, participant_id).await
    }

    async fn add_private_performance_feedback(&self, ctx: &Context<'_>, input: AddPrivatePerformanceFeedbackInput) -> Result<PerformanceFeedbackDto> {
        let claims = require_client_claims(ctx)?; require_manage(claims)?; let tenant_id = require_tenant_id(ctx)?; let participant_id = parse_id(&input.participant_id)?; let comments = validate_text(&input.comments, 4000)?; let db = tenant_db(ctx, tenant_id).await?;
        let participant = performance_workflow::load_participant(&db, tenant_id, participant_id).await.map_err(KabiPayError::into_graphql)?; let goal_id = input.goal_id.as_ref().map(parse_id).transpose()?;
        if let Some(goal_id) = goal_id {
            let goal_exists = goal::Entity::find_by_id(goal_id)
                .filter(goal::Column::TenantId.eq(tenant_id))
                .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
                .filter(goal::Column::EmployeeId.eq(participant.employee_id))
                .one(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
                .is_some();
            if !goal_exists {
                return Err(KabiPayError::Forbidden("Feedback goal does not belong to this participant".into()).into_graphql());
            }
        }
        let row = continuous_feedback::ActiveModel { id:Set(Uuid::new_v4()),tenant_id:Set(tenant_id),review_cycle_id:Set(Some(participant.review_cycle_id)),goal_id:Set(goal_id),reviewer_employee_id:Set(claims.employee_id),reviewee_employee_id:Set(participant.employee_id),visibility:Set("HR_ONLY".into()),observation_date:Set(input.observation_date),comments:Set(comments),created_by_user_id:Set(claims.sub),created_at:Set(chrono::Utc::now()) }.insert(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?; Ok(row.into())
    }

    async fn save_performance_kpi_target(&self, ctx: &Context<'_>, input: SavePerformanceKpiTargetInput) -> Result<PerformanceGoalKpiDto> {
        let claims = require_client_claims(ctx)?; let tenant_id = require_tenant_id(ctx)?; let participant_id = parse_id(&input.participant_id)?; let goal_id = parse_id(&input.goal_id)?; let metric_name = validate_text(&input.metric_name, 255)?; let target_value = input.target_value.as_deref().map(|v| parse_decimal(v, "KPI target")).transpose()?; let unit = optional_text(input.unit, 100)?; let db = tenant_db(ctx, tenant_id).await?; let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?; if stage != "GOAL_SETTING" { return Err(KabiPayError::Validation("KPI targets can only change during goal setting".into()).into_graphql()); }
        let goal = goal::Entity::find_by_id(goal_id).filter(goal::Column::TenantId.eq(tenant_id)).filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id)).filter(goal::Column::EmployeeId.eq(participant.employee_id)).one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.ok_or_else(|| KabiPayError::NotFound { entity:"performance goal",id:goal_id.to_string() }.into_graphql())?;
        goal_actor_can_manage(claims, &participant, true, Some(&goal.status))?;
        let id = input.id.as_ref().map(parse_id).transpose()?.unwrap_or_else(Uuid::new_v4);
        let existing = kpi::Entity::find_by_id(id).filter(kpi::Column::TenantId.eq(tenant_id)).one(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        if existing.as_ref().is_some_and(|kpi| kpi.goal_id != goal_id) {
            return Err(KabiPayError::Validation("A KPI cannot be moved to another goal".into()).into_graphql());
        }
        let is_update = existing.is_some();
        let now=chrono::Utc::now(); let mut row=existing.map(IntoActiveModel::into_active_model).unwrap_or_else(|| kpi::ActiveModel{id:Set(id),tenant_id:Set(tenant_id),goal_id:Set(goal_id),actual_value:Set(None),measurement_date:Set(None),evidence:Set(None),comment:Set(None),created_at:Set(now),..Default::default()}); row.metric_name=Set(metric_name);row.target_value=Set(target_value);row.unit=Set(unit);row.updated_at=Set(now);let saved=if is_update{row.update(&txn).await}else{row.insert(&txn).await}.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let mut goal = goal.into_active_model(); goal.status=Set("PROPOSED".into());goal.updated_at=Set(now);goal.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;Ok(kpi_dto(saved))
    }
    async fn submit_performance_kpi_actual(&self, ctx: &Context<'_>, input: SubmitPerformanceKpiActualInput) -> Result<PerformanceGoalKpiDto> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&input.participant_id)?;
        let kpi_id = parse_id(&input.goal_kpi_id)?;
        let actual = input.actual_value.as_deref().map(|value| parse_decimal(value, "KPI actual")).transpose()?;
        let evidence = optional_text(input.evidence, 4000)?;
        let comment = optional_text(input.comment, 4000)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        if participant.response_revision != input.expected_revision {
            return Err(KabiPayError::Conflict("Performance review changed; refresh before submitting KPI actuals".into()).into_graphql());
        }
        require_kpi_actual_authority(claims, &participant, &stage)?;
        let existing = load_kpi_for_participant(&txn, tenant_id, &participant, kpi_id).await?;
        let mut row = existing.into_active_model();
        row.actual_value = Set(actual);
        row.evidence = Set(evidence);
        row.comment = Set(comment);
        row.measurement_date = Set(input.measurement_date);
        row.updated_at = Set(chrono::Utc::now());
        let saved = row.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::snapshot_revision(&txn, tenant_id, participant_id).await.map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(kpi_dto(saved))
    }

    async fn delete_performance_goal_kpi(&self, ctx: &Context<'_>, participant_id: ID, goal_kpi_id: ID) -> Result<bool> {
        let claims = require_client_claims(ctx)?;
        let tenant_id = require_tenant_id(ctx)?;
        let participant_id = parse_id(&participant_id)?;
        let kpi_id = parse_id(&goal_kpi_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let (participant, stage) = locked_goal_context(&txn, tenant_id, participant_id).await?;
        if stage != "GOAL_SETTING" {
            return Err(KabiPayError::Validation("KPI targets can only be deleted during goal setting".into()).into_graphql());
        }
        let existing = load_kpi_for_participant(&txn, tenant_id, &participant, kpi_id).await?;
        let goal = goal::Entity::find_by_id(existing.goal_id).filter(goal::Column::TenantId.eq(tenant_id)).one(&txn).await
            .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance goal", id: existing.goal_id.to_string() }.into_graphql())?;
        goal_actor_can_manage(claims, &participant, true, Some(&goal.status))?;
        kpi::Entity::delete_by_id(kpi_id).filter(kpi::Column::TenantId.eq(tenant_id)).exec(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let now = chrono::Utc::now();
        let mut goal = goal.into_active_model();
        goal.status = Set("PROPOSED".into());
        goal.updated_at = Set(now);
        goal.update(&txn).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(true)
    }
    async fn retry_performance_exception(&self, ctx: &Context<'_>, exception_id: ID) -> Result<PerformanceAdminExceptionDto> {
        let claims = require_client_claims(ctx)?;
        require_manage(claims)?;
        let tenant_id = require_tenant_id(ctx)?;
        let exception_id = parse_id(&exception_id)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        performance_administration::retry_exception(&txn, tenant_id, exception_id, claims.sub)
            .await
            .map_err(KabiPayError::into_graphql)?;
        txn.commit().await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        let row = db.query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT exception_code,details,resolved_at,created_at FROM performance_admin_exception WHERE tenant_id=$1 AND id=$2",
            [tenant_id.into(), exception_id.into()],
        )).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| KabiPayError::NotFound { entity: "performance exception", id: exception_id.to_string() }.into_graphql())?;
        Ok(PerformanceAdminExceptionDto {
            id: exception_id.to_string().into(),
            exception_code: row.try_get("", "exception_code").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            details: row.try_get("", "details").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            resolved_at: row.try_get("", "resolved_at").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            created_at: row.try_get("", "created_at").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        })
    }
}

pub struct AdministrationQueryRoot;

#[Object(name = "PerformanceAdministrationQueries")]
impl AdministrationQueryRoot {
    async fn performance_population_options(
        &self,
        ctx: &Context<'_>,
        input: PerformancePopulationOptionsInput,
    ) -> Result<PerformancePopulationOptionsDto> {
        super::administration_population::population_options(ctx, input).await
    }
    async fn performance_review_revision(
        &self,
        ctx: &Context<'_>,
        participant_id: ID,
        revision: i32,
    ) -> Result<PerformanceReviewRevisionDetailDto> {
        super::administration_history::review_revision(ctx, participant_id, revision).await
    }
    async fn performance_admin_cycles(
        &self,
        ctx: &Context<'_>,
        input: PerformanceAdminCyclesInput,
    ) -> Result<PerformanceAdminCyclePageDto> {
        super::administration_cycles::admin_cycles(ctx, input).await
    }
    async fn performance_cycle_administration(
        &self,
        ctx: &Context<'_>,
        review_cycle_id: ID,
        cursor: Option<String>,
        limit: Option<i32>,
    ) -> Result<PerformanceCycleAdministrationDto> {
        super::administration_cycles::cycle_administration(
            ctx,
            review_cycle_id,
            cursor,
            limit,
        )
        .await
    }
    async fn performance_program_policy(
        &self,
        ctx: &Context<'_>,
        performance_program_id: ID,
    ) -> Result<PerformanceProgramPolicyDto> {
        super::administration_policy::program_policy(ctx, performance_program_id).await
    }
    async fn performance_goal_kpis(&self, ctx: &Context<'_>, participant_id: ID, goal_id: Option<ID>) -> Result<Vec<PerformanceGoalKpiDto>> {
        let claims=require_client_claims(ctx)?;let tenant_id=require_tenant_id(ctx)?;let participant_id=parse_id(&participant_id)?;let db=tenant_db(ctx,tenant_id).await?;let participant=performance_workflow::load_participant(&db,tenant_id,participant_id).await.map_err(KabiPayError::into_graphql)?;
        goal_actor_can_manage(claims,&participant,false,None)?;let goal_ids=goal::Entity::find().filter(goal::Column::TenantId.eq(tenant_id)).filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id)).filter(goal::Column::EmployeeId.eq(participant.employee_id)).all(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.into_iter().map(|row|row.id).collect::<Vec<_>>();let mut query=kpi::Entity::find().filter(kpi::Column::TenantId.eq(tenant_id)).filter(kpi::Column::GoalId.is_in(goal_ids));if let Some(goal_id)=goal_id { query=query.filter(kpi::Column::GoalId.eq(parse_id(&goal_id)?)); } query.all(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql).map(|rows| rows.into_iter().map(kpi_dto).collect())
    }
    async fn private_performance_feedback(&self, ctx:&Context<'_>, input:PrivatePerformanceFeedbackInput)->Result<PerformanceFeedbackPageDto> {
        let claims=require_client_claims(ctx)?;require_manage(claims)?;let tenant_id=require_tenant_id(ctx)?;let participant_id=parse_id(&input.participant_id)?;let db=tenant_db(ctx,tenant_id).await?;let participant=performance_workflow::load_participant(&db,tenant_id,participant_id).await.map_err(KabiPayError::into_graphql)?;let limit=page_limit(input.limit)?;
        let mut query=continuous_feedback::Entity::find().filter(continuous_feedback::Column::TenantId.eq(tenant_id)).filter(continuous_feedback::Column::RevieweeEmployeeId.eq(participant.employee_id)).filter(continuous_feedback::Column::ReviewCycleId.eq(participant.review_cycle_id)).filter(continuous_feedback::Column::Visibility.eq("HR_ONLY"));
        if let Some((created_at, id)) = parse_feedback_cursor(input.cursor.as_deref())? { query=query.filter(Condition::any().add(continuous_feedback::Column::CreatedAt.lt(created_at)).add(Condition::all().add(continuous_feedback::Column::CreatedAt.eq(created_at)).add(continuous_feedback::Column::Id.lt(id)))); }
        let rows=query.order_by_desc(continuous_feedback::Column::CreatedAt).order_by_desc(continuous_feedback::Column::Id).limit(limit+1).all(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;let has_next=rows.len() as u64>limit;let rows=rows.into_iter().take(limit as usize).collect::<Vec<_>>();let next_cursor=has_next.then(||rows.last().map(|row|format!("{}|{}",row.created_at.to_rfc3339(),row.id))).flatten();Ok(PerformanceFeedbackPageDto{items:rows.into_iter().map(Into::into).collect(),next_cursor})
    }
}

#[cfg(test)]
#[path = "administration_tests.rs"]
mod administration_tests;
