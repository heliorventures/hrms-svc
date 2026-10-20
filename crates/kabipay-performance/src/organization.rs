use crate::{
    resolvers::mutation::{self, AnswerRole, AppraisalAnswerInput},
    services::{performance_administration, performance_lifecycle, performance_workflow},
};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0018_performance::goal,
    d0075_performance_appraisal_lifecycle::{continuous_feedback, performance_participant},
};
use kabipay_organization_protocol::*;
use kabipay_organization_tenant::{statement, unavailable, EmptyPayload, SourceAdapter};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, IntoActiveModel, QueryFilter, QuerySelect, Set,
};
use uuid::Uuid;
#[derive(Clone)]
pub struct PerformanceAdapter;
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct FeedbackInput {
    participant_id: Option<Uuid>,
    goal_id: Option<Uuid>,
    observation_date: chrono::NaiveDate,
    comments: String,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerInput {
    question_id: Uuid,
    text_answer: Option<String>,
    selected_option_ids: Vec<Uuid>,
    rating: Option<String>,
}
#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewInput {
    answers: Vec<AnswerInput>,
    final_rating: String,
    performance_band: Option<String>,
    expected_revision: i32,
}
fn domain(error: async_graphql::Error) -> KabiPayError {
    KabiPayError::Validation(error.message)
}
async fn manager(
    tx: &DatabaseTransaction,
    p: &RuntimeAuthorization,
    participant: Uuid,
) -> KabiPayResult<()> {
    if p.authority_kind == AuthorityKind::OrganizationGrant && p.employee_scope == "ALL" {
        return Ok(());
    }
    let row=tx.query_one(statement("SELECT 1 AS n FROM organization_review_manager s WHERE s.tenant_id=$1 AND s.participant_id=$2 AND s.principal_id=$3 AND s.assignment_id=$4 AND s.assignment_version=$5 AND s.assignment_id=(SELECT a.id FROM management_assignment a WHERE a.tenant_id=$1 AND a.subject_employee_id=$6 AND a.status='ACTIVE' AND a.relationship_type IN ('PRIMARY','REVIEWER') AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND EXISTS(SELECT 1 FROM assignment_capability c WHERE c.tenant_id=a.tenant_id AND c.assignment_id=a.id AND c.capability_code='performance:evaluate') ORDER BY CASE WHEN a.relationship_type='PRIMARY' THEN 0 ELSE 1 END,a.graph_version DESC,a.id LIMIT 1) FOR SHARE OF s",vec![p.tenant_id.into(),participant.into(),p.principal_id.into(),p.authority_id.into(),p.authority_version.into(),p.employee_id.into()])).await?;
    if row.is_none() {
        return Err(unavailable());
    }
    Ok(())
}
impl SourceAdapter for PerformanceAdapter {
    fn service(&self) -> &'static str {
        "kabipay-performance"
    }
    async fn prepare(
        &self,
        tx: &DatabaseTransaction,
        tenant: Uuid,
        c: &OrganizationCommand,
    ) -> KabiPayResult<()> {
        if c.operation == OrganizationOperation::FeedbackCreate {
            if tx
                .query_one(statement(
                    "SELECT id FROM employee WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
                    vec![tenant.into(), c.resource_id.into()],
                ))
                .await?
                .is_none()
            {
                return Err(unavailable());
            }
        }
        if matches!(
            c.operation,
            OrganizationOperation::ReviewDetail | OrganizationOperation::ReviewSubmit
        ) {
            let initial=tx.query_one(statement("SELECT review_cycle_id,employee_id FROM performance_participant WHERE tenant_id=$1 AND id=$2 AND NOT is_excluded",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
            let cycle: Uuid = initial.try_get("", "review_cycle_id")?;
            let employee: Uuid = initial.try_get("", "employee_id")?;
            performance_workflow::locked_cycle_stage(tx, tenant, cycle).await?;
            let locked=tx.query_one(statement("SELECT review_cycle_id,employee_id FROM performance_participant WHERE tenant_id=$1 AND id=$2 AND NOT is_excluded FOR UPDATE",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
            if locked.try_get::<Uuid>("", "review_cycle_id")? != cycle
                || locked.try_get::<Uuid>("", "employee_id")? != employee
            {
                return Err(KabiPayError::Conflict(
                    "Performance review changed while acquiring its cycle lock; retry the request"
                        .into(),
                ));
            }
        }
        Ok(())
    }
    async fn employee(
        &self,
        db: &DatabaseConnection,
        tenant: Uuid,
        c: &OrganizationCommand,
    ) -> KabiPayResult<Option<Uuid>> {
        if c.operation == OrganizationOperation::FeedbackCreate {
            let row=db.query_one(statement("SELECT id FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
            return Ok(Some(row.try_get("", "id")?));
        }
        let row=db.query_one(statement("SELECT employee_id FROM performance_participant WHERE tenant_id=$1 AND id=$2 AND NOT is_excluded",vec![tenant.into(),c.resource_id.into()])).await?.ok_or_else(unavailable)?;
        Ok(Some(row.try_get("", "employee_id")?))
    }
    async fn execute(
        &self,
        tx: &DatabaseTransaction,
        _: &DatabaseConnection,
        p: &kabipay_organization_tenant::VerifiedRuntimeAuthorization,
        c: &OrganizationCommand,
    ) -> KabiPayResult<serde_json::Value> {
        if c.operation == OrganizationOperation::FeedbackCreate {
            return create_feedback(tx, p, c).await;
        }
        let initial = performance_participant::Entity::find_by_id(c.resource_id)
            .filter(performance_participant::Column::TenantId.eq(p.tenant_id))
            .one(tx)
            .await?
            .ok_or_else(unavailable)?;
        let stage =
            performance_workflow::locked_cycle_stage(tx, p.tenant_id, initial.review_cycle_id)
                .await?;
        let participant = performance_participant::Entity::find_by_id(c.resource_id)
            .filter(performance_participant::Column::TenantId.eq(p.tenant_id))
            .lock_exclusive()
            .one(tx)
            .await?
            .ok_or_else(unavailable)?;
        if participant.review_cycle_id != initial.review_cycle_id
            || participant.is_excluded
            || p.employee_id != Some(participant.employee_id)
        {
            return Err(unavailable());
        }
        manager(tx, p, participant.id).await?;
        match c.operation {
            OrganizationOperation::ReviewDetail => {
                let _: EmptyPayload = serde_json::from_value(c.payload.clone())?;
                let detail =
                    performance_workflow::load_review_detail(tx, p.tenant_id, participant).await?;
                Ok(serde_json::to_value(detail)?)
            }
            OrganizationOperation::ReviewSubmit => {
                let input: ReviewInput = serde_json::from_value(c.payload.clone())?;
                mutation::require_appraisal_submission_state(
                    &participant,
                    initial.review_cycle_id,
                    true,
                )
                .map_err(domain)?;
                mutation::require_expected_revision(
                    participant.response_revision,
                    Some(input.expected_revision),
                )
                .map_err(domain)?;
                if participant.manager_submitted_at.is_some() || stage != "MANAGER_REVIEW" {
                    return Err(KabiPayError::Conflict(
                        "Manager appraisal is not awaiting this submission".into(),
                    ));
                }
                let rating =
                    mutation::parse_decimal(&input.final_rating, "Final rating").map_err(domain)?;
                let band = mutation::optional_text(input.performance_band, 50).map_err(domain)?;
                let program = performance_workflow::program_for_cycle(
                    tx,
                    p.tenant_id,
                    participant.review_cycle_id,
                )
                .await?;
                performance_lifecycle::validate_rating(
                    rating,
                    program.rating_min,
                    program.rating_max,
                )
                .map_err(KabiPayError::Validation)?;
                let answers = input
                    .answers
                    .into_iter()
                    .map(|a| AppraisalAnswerInput {
                        question_id: a.question_id.to_string().into(),
                        text_answer: a.text_answer,
                        selected_option_ids: a
                            .selected_option_ids
                            .into_iter()
                            .map(|id| id.to_string().into())
                            .collect(),
                        rating: a.rating,
                    })
                    .collect();
                mutation::save_appraisal_answers(
                    tx,
                    p.tenant_id,
                    &participant,
                    answers,
                    AnswerRole::Manager,
                )
                .await
                .map_err(domain)?;
                let mut model = participant.into_active_model();
                model.manager_submitted_at = Set(Some(chrono::Utc::now()));
                model.manager_rating = Set(Some(rating));
                model.manager_performance_band = Set(band.clone());
                model.final_rating = Set((!program.include_calibration).then_some(rating));
                model.performance_band =
                    Set((!program.include_calibration).then_some(band).flatten());
                model.status = Set("MANAGER_SUBMITTED".into());
                model.updated_at = Set(chrono::Utc::now());
                let participant = model.update(tx).await?;
                performance_administration::snapshot_revision(tx, p.tenant_id, participant.id)
                    .await?;
                Ok(
                    serde_json::json!({"id":participant.id,"status":participant.status,"response_revision":participant.response_revision}),
                )
            }
            _ => Err(unavailable()),
        }
    }
}

async fn create_feedback(
    tx: &DatabaseTransaction,
    p: &kabipay_organization_tenant::VerifiedRuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<serde_json::Value> {
    let input: FeedbackInput = serde_json::from_value(c.payload.clone())?;
    let comments = mutation::validate_text(&input.comments, 4000).map_err(domain)?;
    if p.employee_id != Some(c.resource_id) {
        return Err(unavailable());
    }
    let cycle = if let Some(id) = input.participant_id {
        let participant = performance_participant::Entity::find_by_id(id)
            .filter(performance_participant::Column::TenantId.eq(p.tenant_id))
            .filter(performance_participant::Column::EmployeeId.eq(c.resource_id))
            .filter(performance_participant::Column::IsExcluded.eq(false))
            .lock_exclusive()
            .one(tx)
            .await?
            .ok_or_else(unavailable)?;
        if let Some(goal_id) = input.goal_id {
            if goal::Entity::find_by_id(goal_id)
                .filter(goal::Column::TenantId.eq(p.tenant_id))
                .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
                .filter(goal::Column::EmployeeId.eq(c.resource_id))
                .one(tx)
                .await?
                .is_none()
            {
                return Err(KabiPayError::Validation(
                    "Feedback goal must belong to this employee review".into(),
                ));
            }
        }
        Some(participant.review_cycle_id)
    } else {
        if input.goal_id.is_some() {
            return Err(KabiPayError::Validation(
                "A review participant is required for goal feedback".into(),
            ));
        }
        None
    };
    let feedback = continuous_feedback::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(p.tenant_id),
        review_cycle_id: Set(cycle),
        goal_id: Set(input.goal_id),
        reviewer_employee_id: Set(None),
        reviewee_employee_id: Set(c.resource_id),
        visibility: Set("EMPLOYEE_VISIBLE".into()),
        observation_date: Set(input.observation_date),
        comments: Set(comments),
        created_by_user_id: Set(p.local_user_id()),
        created_at: Set(chrono::Utc::now()),
    }
    .insert(tx)
    .await?;
    tx.execute(statement("INSERT INTO organization_business_actor(tenant_id,resource_type,resource_id,principal_id,command_id,action) VALUES($1,'continuous_feedback',$2,$3,$4,'feedback:write')",vec![p.tenant_id.into(),feedback.id.into(),p.principal_id.into(),c.command_id.into()])).await?;
    Ok(
        serde_json::json!({"id":feedback.id,"employee_id":c.resource_id,"participant_id":input.participant_id,"status":"CREATED"}),
    )
}

#[cfg(test)]
#[path = "organization_tests.rs"]
mod tests;
