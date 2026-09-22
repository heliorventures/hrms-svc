use async_graphql::{Context, Result, ID};
use chrono::NaiveDate;
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id},
    KabiPayError,
};
#[cfg(not(test))]
use kabipay_common::subgraph::tenant_db;
#[cfg(test)]
use super::concurrency_tests::tenant_db;
use kabipay_db_entities::tenant::{
    d0018_performance::goal,
    d0075_performance_appraisal_lifecycle::appraisal_answer,
};
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement,
    TryGetable,
};
use serde_json::Value;
use uuid::Uuid;

use super::{
    administration::kpi_dto,
    administration_types::{
        PerformanceCalibrationDecisionDto, PerformanceGoalKpiDto,
        PerformanceParticipantAdministrationDto, PerformanceReviewRevisionDetailDto,
        PerformanceReviewRevisionDto,
    },
    mutation::{parse_id, require_manage},
};
use crate::services::performance_workflow;

pub(super) async fn review_revision(
    ctx: &Context<'_>,
    participant_id: ID,
    revision: i32,
) -> Result<PerformanceReviewRevisionDetailDto> {
    let claims = require_client_claims(ctx)?;
    require_manage(claims)?;
    let tenant_id = require_tenant_id(ctx)?;
    let participant_id = parse_id(&participant_id)?;
    if revision < 1 {
        return Err(KabiPayError::Validation("Revision must be positive".into()).into_graphql());
    }

    let db = tenant_db(ctx, tenant_id).await?;
    let participant = load_admin_participant(&db, tenant_id, participant_id).await?;
    let response_revision = participant.response_revision;
    let review = participant
        .revisions
        .into_iter()
        .find(|item| item.revision == revision)
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance review revision",
            id: revision.to_string(),
        }.into_graphql())?;
    let answers = appraisal_answer::Entity::find()
        .filter(appraisal_answer::Column::TenantId.eq(tenant_id))
        .filter(appraisal_answer::Column::PerformanceParticipantId.eq(participant_id))
        .filter(appraisal_answer::Column::Revision.eq(revision))
        .all(&db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .into_iter()
        .map(Into::into)
        .collect();
    let calibrations = calibration_decisions(&db, tenant_id, participant_id, revision).await?;
    let kpis = if response_revision == revision {
        current_revision_kpis(&db, tenant_id, participant_id).await?
    } else {
        revision_snapshot_kpis(&db, tenant_id, participant_id, revision).await?
    };
    let acknowledgement_comment = acknowledgement_comment(&db, tenant_id, participant_id, revision).await?;

    Ok(PerformanceReviewRevisionDetailDto {
        review,
        answers,
        kpis,
        calibrations,
        acknowledgement_comment,
    })
}

pub(super) async fn load_admin_participant(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> Result<PerformanceParticipantAdministrationDto> {
    let participant = performance_workflow::load_participant(db, tenant_id, participant_id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let employee_name = employee_name(db, tenant_id, participant.employee_id).await?;
    let revisions = participant_revisions(db, tenant_id, participant_id).await?;

    Ok(PerformanceParticipantAdministrationDto {
        participant_id: participant.id.to_string().into(),
        employee_id: participant.employee_id.to_string().into(),
        employee_name,
        manager_employee_id: participant.manager_employee_id.map(|id| id.to_string().into()),
        status: participant.status,
        is_excluded: participant.is_excluded,
        exclusion_reason: participant.exclusion_reason,
        response_revision: participant.response_revision,
        self_submitted_at: participant.self_submitted_at,
        manager_submitted_at: participant.manager_submitted_at,
        acknowledged_at: participant.acknowledged_at,
        manager_rating: participant.manager_rating.map(|value| value.to_string()),
        final_rating: participant.final_rating.map(|value| value.to_string()),
        performance_band: participant.performance_band,
        calibration_provenance: participant.calibration_provenance,
        revisions,
    })
}

async fn employee_name(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
) -> Result<String> {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT BTRIM(CONCAT(first_name,' ',last_name)) AS employee_name FROM employee WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), employee_id.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .ok_or_else(|| KabiPayError::NotFound {
        entity: "employee",
        id: employee_id.to_string(),
    }.into_graphql())?
    .try_get("", "employee_name")
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)
}

async fn participant_revisions(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> Result<Vec<PerformanceReviewRevisionDto>> {
    db.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT revision,reopened_at,reopened_by_user_id,reopen_reason,correction_stage,self_submitted_at,manager_submitted_at,acknowledged_at,manager_rating,final_rating,performance_band,calibration_provenance FROM performance_participant_revision WHERE tenant_id=$1 AND performance_participant_id=$2 ORDER BY revision DESC",
        [tenant_id.into(), participant_id.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .into_iter()
    .map(revision_dto)
    .collect()
}

fn revision_dto(row: sea_orm::QueryResult) -> Result<PerformanceReviewRevisionDto> {
    Ok(PerformanceReviewRevisionDto {
        revision: row
            .try_get("", "revision")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        reopened_at: row
            .try_get("", "reopened_at")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        reopened_by_user_id: row
            .try_get::<Option<Uuid>>("", "reopened_by_user_id")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .map(|id| id.to_string().into()),
        reopen_reason: row
            .try_get("", "reopen_reason")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        correction_stage: row
            .try_get("", "correction_stage")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        self_submitted_at: row
            .try_get("", "self_submitted_at")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        manager_submitted_at: row
            .try_get("", "manager_submitted_at")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        acknowledged_at: row
            .try_get("", "acknowledged_at")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        manager_rating: row
            .try_get::<Option<rust_decimal::Decimal>>("", "manager_rating")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .map(|value| value.to_string()),
        final_rating: row
            .try_get::<Option<rust_decimal::Decimal>>("", "final_rating")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .map(|value| value.to_string()),
        performance_band: row
            .try_get("", "performance_band")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        calibration_provenance: row
            .try_get("", "calibration_provenance")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
    })
}

async fn calibration_decisions(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
    revision: i32,
) -> Result<Vec<PerformanceCalibrationDecisionDto>> {
    db.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id,revision,final_rating,performance_band,reason,decided_by_user_id,decided_at FROM performance_calibration_decision WHERE tenant_id=$1 AND performance_participant_id=$2 AND revision=$3 ORDER BY decided_at",
        [tenant_id.into(), participant_id.into(), revision.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .into_iter()
    .map(|row| Ok(PerformanceCalibrationDecisionDto {
        id: row.try_get::<Uuid>("", "id").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.to_string().into(),
        revision: row.try_get("", "revision").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        final_rating: row.try_get::<rust_decimal::Decimal>("", "final_rating").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.to_string(),
        performance_band: row.try_get("", "performance_band").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        reason: row.try_get("", "reason").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        decided_by_user_id: row.try_get::<Uuid>("", "decided_by_user_id").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.to_string().into(),
        decided_at: row.try_get("", "decided_at").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
    }))
    .collect()
}

async fn current_revision_kpis(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
) -> Result<Vec<PerformanceGoalKpiDto>> {
    let participant = performance_workflow::load_participant(db, tenant_id, participant_id)
        .await
        .map_err(KabiPayError::into_graphql)?;
    let goal_ids = goal::Entity::find()
        .filter(goal::Column::TenantId.eq(tenant_id))
        .filter(goal::Column::ReviewCycleId.eq(participant.review_cycle_id))
        .filter(goal::Column::EmployeeId.eq(participant.employee_id))
        .all(db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .into_iter()
        .map(|goal| goal.id)
        .collect::<Vec<_>>();
    kabipay_db_entities::tenant::d0018_performance::kpi::Entity::find()
        .filter(kabipay_db_entities::tenant::d0018_performance::kpi::Column::TenantId.eq(tenant_id))
        .filter(kabipay_db_entities::tenant::d0018_performance::kpi::Column::GoalId.is_in(goal_ids))
        .all(db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)
        .map(|rows| rows.into_iter().map(kpi_dto).collect())
}

async fn acknowledgement_comment(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
    revision: i32,
) -> Result<Option<String>> {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT acknowledgement_comment FROM performance_participant_revision WHERE tenant_id=$1 AND performance_participant_id=$2 AND revision=$3",
        [tenant_id.into(), participant_id.into(), revision.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .map(|row| row.try_get("", "acknowledgement_comment"))
    .transpose()
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)
}

async fn revision_snapshot_kpis(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    participant_id: Uuid,
    revision: i32,
) -> Result<Vec<PerformanceGoalKpiDto>> {
    let snapshot: Option<Value> = db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT kpi_snapshot FROM performance_participant_revision WHERE tenant_id=$1 AND performance_participant_id=$2 AND revision=$3",
        [tenant_id.into(), participant_id.into(), revision.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .map(|row| row.try_get::<Option<Value>>("", "kpi_snapshot"))
    .transpose()
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?;

    snapshot
        .as_ref()
        .and_then(Value::as_array)
        .map(|items| items.iter().map(snapshot_kpi_dto).collect())
        .unwrap_or_else(|| Ok(Vec::new()))
}

fn snapshot_kpi_dto(value: &Value) -> Result<PerformanceGoalKpiDto> {
    let object = value.as_object().ok_or_else(|| {
        KabiPayError::Validation("Stored performance KPI snapshot is invalid".into()).into_graphql()
    })?;
    let required = |field| snapshot_string(object, field).ok_or_else(|| {
        KabiPayError::Validation("Stored performance KPI snapshot is incomplete".into()).into_graphql()
    });
    let measurement_date = snapshot_string(object, "measurementDate")
        .map(|date| NaiveDate::parse_from_str(&date, "%Y-%m-%d").map_err(|_| {
            KabiPayError::Validation("Stored performance KPI measurement date is invalid".into()).into_graphql()
        }))
        .transpose()?;

    Ok(PerformanceGoalKpiDto {
        id: required("id")?.into(),
        goal_id: required("goalId")?.into(),
        metric_name: required("metricName")?,
        target_value: snapshot_string(object, "targetValue"),
        actual_value: snapshot_string(object, "actualValue"),
        unit: snapshot_string(object, "unit"),
        evidence: snapshot_string(object, "evidence"),
        comment: snapshot_string(object, "comment"),
        measurement_date,
    })
}

fn snapshot_string(value: &serde_json::Map<String, Value>, field: &str) -> Option<String> {
    value.get(field).and_then(|value| match value {
        Value::String(value) => Some(value.clone()),
        Value::Number(value) => Some(value.to_string()),
        _ => None,
    })
}
