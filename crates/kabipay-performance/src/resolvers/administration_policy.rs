use async_graphql::{Context, Result, ID};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id},
    KabiPayError,
};
#[cfg(not(test))]
use kabipay_common::subgraph::tenant_db;
#[cfg(test)]
use super::concurrency_tests::tenant_db;
use kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::performance_program;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseBackend, EntityTrait, QueryFilter, Statement,
    QuerySelect, TransactionTrait, TryGetable,
};
use uuid::Uuid;

use super::{
    administration_types::{PerformanceProgramPolicyDto, PerformanceProgramPolicyInput},
    mutation::{parse_id, require_manage},
};

pub(super) async fn save_program_policy(
    ctx: &Context<'_>,
    input: PerformanceProgramPolicyInput,
) -> Result<PerformanceProgramPolicyDto> {
    let claims = require_client_claims(ctx)?;
    require_manage(claims)?;
    let tenant_id = require_tenant_id(ctx)?;
    let program_id = parse_id(&input.performance_program_id)?;
    let mode = validate_population_input(&input)?;
    let selection_ids = selection_ids(&input)?;

    let db = tenant_db(ctx, tenant_id).await?;
    let txn = db
        .begin()
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let program = performance_program::Entity::find_by_id(program_id)
        .filter(performance_program::Column::TenantId.eq(tenant_id))
        .lock_exclusive()
        .one(&txn)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "performance program",
            id: program_id.to_string(),
        }.into_graphql())?;
    if program.status == "ARCHIVED" {
        return Err(KabiPayError::Validation(
            "Archived performance programs cannot be configured".into(),
        ).into_graphql());
    }

    validate_population_selections(&txn, tenant_id, &mode, &selection_ids).await?;

    let now = chrono::Utc::now();
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "INSERT INTO performance_program_policy (performance_program_id,tenant_id,population_mode,goal_setting_due_days,self_review_due_days,manager_review_due_days,calibration_due_days,acknowledgement_due_days,updated_by,updated_at) VALUES ($1,$2,$3,$4,$5,$6,$7,$8,$9,$10) ON CONFLICT (performance_program_id) DO UPDATE SET population_mode=EXCLUDED.population_mode,goal_setting_due_days=EXCLUDED.goal_setting_due_days,self_review_due_days=EXCLUDED.self_review_due_days,manager_review_due_days=EXCLUDED.manager_review_due_days,calibration_due_days=EXCLUDED.calibration_due_days,acknowledgement_due_days=EXCLUDED.acknowledgement_due_days,updated_by=EXCLUDED.updated_by,updated_at=EXCLUDED.updated_at",
        [
            program_id.into(),
            tenant_id.into(),
            mode.clone().into(),
            input.goal_setting_due_days.into(),
            input.self_review_due_days.into(),
            input.manager_review_due_days.into(),
            input.calibration_due_days.into(),
            input.acknowledgement_due_days.into(),
            claims.sub.into(),
            now.into(),
        ],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?;
    txn.execute(Statement::from_sql_and_values(
        DatabaseBackend::Postgres,
        "DELETE FROM performance_program_population WHERE tenant_id=$1 AND performance_program_id=$2",
        [tenant_id.into(), program_id.into()],
    ))
    .await
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?;
    for selection_id in &selection_ids {
        txn.execute(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "INSERT INTO performance_program_population (performance_program_id,tenant_id,selection_id,created_at) VALUES ($1,$2,$3,$4)",
            [
                program_id.into(),
                tenant_id.into(),
                (*selection_id).into(),
                now.into(),
            ],
        ))
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    }
    txn.commit()
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;

    Ok(PerformanceProgramPolicyDto {
        performance_program_id: program_id.to_string().into(),
        archived_at: None,
        population_mode: mode,
        population_ids: selection_ids
            .into_iter()
            .map(|id| id.to_string().into())
            .collect(),
        goal_setting_due_days: input.goal_setting_due_days,
        self_review_due_days: input.self_review_due_days,
        manager_review_due_days: input.manager_review_due_days,
        calibration_due_days: input.calibration_due_days,
        acknowledgement_due_days: input.acknowledgement_due_days,
    })
}

pub(super) async fn program_policy(
    ctx: &Context<'_>,
    performance_program_id: ID,
) -> Result<PerformanceProgramPolicyDto> {
    let claims = require_client_claims(ctx)?;
    require_manage(claims)?;
    let tenant_id = require_tenant_id(ctx)?;
    let program_id = parse_id(&performance_program_id)?;
    let db = tenant_db(ctx, tenant_id).await?;

    let row = db
        .query_one(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT population_mode,goal_setting_due_days,self_review_due_days,manager_review_due_days,calibration_due_days,acknowledgement_due_days,archived_at FROM performance_program_policy WHERE tenant_id=$1 AND performance_program_id=$2",
            [tenant_id.into(), program_id.into()],
        ))
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let population_ids = db
        .query_all(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT selection_id FROM performance_program_population WHERE tenant_id=$1 AND performance_program_id=$2 ORDER BY selection_id",
            [tenant_id.into(), program_id.into()],
        ))
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .into_iter()
        .map(|row| {
            row.try_get::<Uuid>("", "selection_id")
                .map(|id| ID(id.to_string()))
                .map_err(KabiPayError::from)
                .map_err(KabiPayError::into_graphql)
        })
        .collect::<Result<Vec<_>>>()?;

    match row {
        Some(row) => Ok(PerformanceProgramPolicyDto {
            performance_program_id: program_id.to_string().into(),
            archived_at: row.try_get("", "archived_at").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            population_mode: row.try_get("", "population_mode").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            population_ids,
            goal_setting_due_days: row.try_get("", "goal_setting_due_days").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            self_review_due_days: row.try_get("", "self_review_due_days").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            manager_review_due_days: row.try_get("", "manager_review_due_days").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            calibration_due_days: row.try_get("", "calibration_due_days").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
            acknowledgement_due_days: row.try_get("", "acknowledgement_due_days").map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?,
        }),
        None => Ok(PerformanceProgramPolicyDto {
            performance_program_id: program_id.to_string().into(),
            archived_at: None,
            population_mode: "ALL".into(),
            population_ids,
            goal_setting_due_days: None,
            self_review_due_days: None,
            manager_review_due_days: None,
            calibration_due_days: None,
            acknowledgement_due_days: None,
        }),
    }
}

fn validate_population_input(input: &PerformanceProgramPolicyInput) -> Result<String> {
    let mode = input.population_mode.trim().to_ascii_uppercase();
    if !["ALL", "DEPARTMENTS", "LOCATIONS", "EMPLOYEES"].contains(&mode.as_str()) {
        return Err(KabiPayError::Validation(
            "Population mode must be ALL, DEPARTMENTS, LOCATIONS, or EMPLOYEES".into(),
        ).into_graphql());
    }
    if mode == "ALL" && !input.population_ids.is_empty() {
        return Err(KabiPayError::Validation(
            "ALL population mode cannot include selection IDs".into(),
        ).into_graphql());
    }
    if mode != "ALL" && input.population_ids.is_empty() {
        return Err(KabiPayError::Validation(
            "A selected population mode requires at least one tenant ID".into(),
        ).into_graphql());
    }
    if [
        input.goal_setting_due_days,
        input.self_review_due_days,
        input.manager_review_due_days,
        input.calibration_due_days,
        input.acknowledgement_due_days,
    ]
    .iter()
    .flatten()
    .any(|days| *days < 0)
    {
        return Err(KabiPayError::Validation(
            "Stage due days cannot be negative".into(),
        ).into_graphql());
    }
    Ok(mode)
}

fn selection_ids(input: &PerformanceProgramPolicyInput) -> Result<Vec<Uuid>> {
    let mut selection_ids = input
        .population_ids
        .iter()
        .map(parse_id)
        .collect::<Result<Vec<_>>>()?;
    selection_ids.sort_unstable();
    selection_ids.dedup();
    Ok(selection_ids)
}

async fn validate_population_selections(
    txn: &sea_orm::DatabaseTransaction,
    tenant_id: Uuid,
    mode: &str,
    selection_ids: &[Uuid],
) -> Result<()> {
    let table = match mode {
        "DEPARTMENTS" => "department",
        "LOCATIONS" => "location",
        "EMPLOYEES" => "employee",
        _ => "",
    };
    for selection_id in selection_ids {
        if table.is_empty() {
            continue;
        }
        let exists = txn
            .query_one(Statement::from_sql_and_values(
                DatabaseBackend::Postgres,
                &format!("SELECT id FROM {table} WHERE tenant_id=$1 AND id=$2"),
                [tenant_id.into(), (*selection_id).into()],
            ))
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .is_some();
        if !exists {
            return Err(KabiPayError::Validation(
                "Population selections must belong to this tenant and selected mode".into(),
            ).into_graphql());
        }
    }
    Ok(())
}
