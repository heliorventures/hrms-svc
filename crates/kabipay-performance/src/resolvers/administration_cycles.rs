use async_graphql::{Context, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id},
    KabiPayError,
};
#[cfg(not(test))]
use kabipay_common::subgraph::tenant_db;
#[cfg(test)]
use super::concurrency_tests::tenant_db;
use kabipay_db_entities::tenant::{
    d0018_performance::review_cycle,
    d0075_performance_appraisal_lifecycle::performance_participant,
};
use sea_orm::{
    ColumnTrait, Condition, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter,
    QueryOrder, QuerySelect, Statement, TryGetable,
};
use uuid::Uuid;

use super::{
    administration_history::load_admin_participant,
    administration_pagination::{cycle_cursor, page_limit, uuid_cursor},
    administration_types::{
        PerformanceAdminCycleDto, PerformanceAdminCyclePageDto, PerformanceAdminCyclesInput,
        PerformanceAdminExceptionDto, PerformanceCycleAdministrationDto,
        PerformanceStageDeadlineDto,
    },
    mutation::{parse_id, require_manage},
};

pub(super) async fn admin_cycles(
    ctx: &Context<'_>,
    input: PerformanceAdminCyclesInput,
) -> Result<PerformanceAdminCyclePageDto> {
    let claims = require_client_claims(ctx)?;
    require_manage(claims)?;
    let tenant_id = require_tenant_id(ctx)?;
    let limit = page_limit(input.limit)?;
    let db = tenant_db(ctx, tenant_id).await?;

    let mut cycles = review_cycle::Entity::find()
        .filter(review_cycle::Column::TenantId.eq(tenant_id));
    if let Some(status) = input.status {
        cycles = cycles.filter(review_cycle::Column::Status.eq(status.trim().to_ascii_uppercase()));
    }
    if let Some(program_id) = input.performance_program_id.as_ref().map(parse_id).transpose()? {
        cycles = cycles.filter(review_cycle::Column::PerformanceProgramId.eq(program_id));
    }
    if let Some((start_date, id)) = cycle_cursor(input.cursor.as_deref())? {
        cycles = cycles.filter(
            Condition::any()
                .add(review_cycle::Column::StartDate.lt(start_date))
                .add(
                    Condition::all()
                        .add(review_cycle::Column::StartDate.eq(start_date))
                        .add(review_cycle::Column::Id.lt(id)),
                ),
        );
    }

    let rows = cycles
        .order_by_desc(review_cycle::Column::StartDate)
        .order_by_desc(review_cycle::Column::Id)
        .limit(limit + 1)
        .all(&db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let has_next = rows.len() as u64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let next_cursor = has_next
        .then(|| rows.last().map(|cycle| format!("{}|{}", cycle.start_date, cycle.id)))
        .flatten();
    let mut items = Vec::with_capacity(rows.len());
    for cycle in rows {
        items.push(admin_cycle(&db, tenant_id, cycle).await?);
    }

    Ok(PerformanceAdminCyclePageDto { items, next_cursor })
}

pub(super) async fn cycle_administration(
    ctx: &Context<'_>,
    review_cycle_id: ID,
    cursor: Option<String>,
    limit: Option<i32>,
) -> Result<PerformanceCycleAdministrationDto> {
    let claims = require_client_claims(ctx)?;
    require_manage(claims)?;
    let tenant_id = require_tenant_id(ctx)?;
    let cycle_id = parse_id(&review_cycle_id)?;
    let limit = page_limit(limit.unwrap_or(50))?;
    let db = tenant_db(ctx, tenant_id).await?;
    let cycle = review_cycle::Entity::find_by_id(cycle_id)
        .filter(review_cycle::Column::TenantId.eq(tenant_id))
        .one(&db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance cycle",
            id: cycle_id.to_string(),
        }.into_graphql())?;
    let info = cycle_info(&db, tenant_id, cycle_id).await?;
    let current_stage = info
        .try_get("", "current_stage")
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let deadlines = deadlines(&info);
    let (participants, next_participant_cursor) = participants(
        &db,
        tenant_id,
        cycle_id,
        uuid_cursor(cursor.as_deref())?,
        limit,
    )
    .await?;
    let exceptions = exceptions(&db, tenant_id, cycle_id).await?;

    Ok(PerformanceCycleAdministrationDto {
        review_cycle: cycle.into(),
        current_stage,
        deadlines,
        participants,
        next_participant_cursor,
        exceptions,
    })
}

async fn admin_cycle(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    cycle: review_cycle::Model,
) -> Result<PerformanceAdminCycleDto> {
    let counts = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT count(*) AS participants,count(*) FILTER (WHERE is_excluded) AS excluded FROM performance_participant WHERE tenant_id=$1 AND review_cycle_id=$2",
            [tenant_id.into(), cycle.id.into()],
        ))
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance cycle",
            id: cycle.id.to_string(),
        }.into_graphql())?;
    let actionable_exception_count = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT count(*) AS count FROM performance_admin_exception WHERE tenant_id=$1 AND review_cycle_id=$2 AND resolved_at IS NULL",
            [tenant_id.into(), cycle.id.into()],
        ))
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance exceptions",
            id: cycle.id.to_string(),
        }.into_graphql())?
        .try_get("", "count")
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let current_stage = cycle_stage(db, tenant_id, cycle.id).await?;

    Ok(PerformanceAdminCycleDto {
        review_cycle: cycle.into(),
        current_stage,
        participant_count: counts.try_get("", "participants").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        excluded_participant_count: counts.try_get("", "excluded").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        actionable_exception_count,
    })
}

async fn cycle_info(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    cycle_id: Uuid,
) -> Result<sea_orm::QueryResult> {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT current_stage,goal_setting_due_date,self_review_due_date,manager_review_due_date,calibration_due_date,acknowledgement_due_date FROM review_cycle WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), cycle_id.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .ok_or_else(|| KabiPayError::NotFound {
        entity: "performance cycle",
        id: cycle_id.to_string(),
    }.into_graphql())
}

fn deadlines(info: &sea_orm::QueryResult) -> Vec<PerformanceStageDeadlineDto> {
    [
        ("GOAL_SETTING", "goal_setting_due_date"),
        ("SELF_REVIEW", "self_review_due_date"),
        ("MANAGER_REVIEW", "manager_review_due_date"),
        ("HR_CALIBRATION", "calibration_due_date"),
        ("EMPLOYEE_ACKNOWLEDGEMENT", "acknowledgement_due_date"),
    ]
    .into_iter()
    .filter_map(|(stage, column)| {
        info.try_get::<Option<chrono::NaiveDate>>("", column)
            .ok()
            .flatten()
            .map(|due_date| PerformanceStageDeadlineDto {
                stage: stage.into(),
                due_date,
            })
    })
    .collect()
}

async fn participants(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    cycle_id: Uuid,
    cursor: Option<Uuid>,
    limit: u64,
) -> Result<(Vec<super::administration_types::PerformanceParticipantAdministrationDto>, Option<String>)> {
    let mut query = performance_participant::Entity::find()
        .filter(performance_participant::Column::TenantId.eq(tenant_id))
        .filter(performance_participant::Column::ReviewCycleId.eq(cycle_id));
    if let Some(cursor) = cursor {
        query = query.filter(performance_participant::Column::Id.gt(cursor));
    }
    let rows = query
        .order_by_asc(performance_participant::Column::Id)
        .limit(limit + 1)
        .all(db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let has_next = rows.len() as u64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let next_cursor = has_next
        .then(|| rows.last().map(|participant| participant.id.to_string()))
        .flatten();
    let mut details = Vec::with_capacity(rows.len());
    for participant in rows {
        details.push(load_admin_participant(db, tenant_id, participant.id).await?);
    }

    Ok((details, next_cursor))
}

async fn exceptions(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    cycle_id: Uuid,
) -> Result<Vec<PerformanceAdminExceptionDto>> {
    db.query_all(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT id,exception_code,details,resolved_at,created_at FROM performance_admin_exception WHERE tenant_id=$1 AND review_cycle_id=$2 AND resolved_at IS NULL ORDER BY created_at,id",
        [tenant_id.into(), cycle_id.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .into_iter()
    .map(|row| Ok(PerformanceAdminExceptionDto {
        id: row.try_get::<Uuid>("", "id").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?.to_string().into(),
        exception_code: row.try_get("", "exception_code").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        details: row.try_get("", "details").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        resolved_at: row.try_get("", "resolved_at").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        created_at: row.try_get("", "created_at").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
    }))
    .collect()
}

async fn cycle_stage(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    cycle_id: Uuid,
) -> Result<String> {
    db.query_one(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "SELECT current_stage FROM review_cycle WHERE tenant_id=$1 AND id=$2",
        [tenant_id.into(), cycle_id.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?
    .ok_or_else(|| KabiPayError::NotFound {
        entity: "performance cycle",
        id: cycle_id.to_string(),
    }.into_graphql())?
    .try_get("", "current_stage")
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)
}
