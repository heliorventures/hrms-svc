use async_graphql::{Context, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id},
    KabiPayError,
};
#[cfg(not(test))]
use kabipay_common::subgraph::tenant_db;
#[cfg(test)]
use super::concurrency_tests::tenant_db;
use sea_orm::{ConnectionTrait, DatabaseBackend, Statement, TryGetable};
use uuid::Uuid;

use super::{
    administration_pagination::{name_cursor, page_limit},
    administration_types::{
        PerformancePopulationOptionDto, PerformancePopulationOptionsDto,
        PerformancePopulationOptionsInput,
    },
    mutation::require_manage,
};

pub(super) async fn population_options(
    ctx: &Context<'_>,
    input: PerformancePopulationOptionsInput,
) -> Result<PerformancePopulationOptionsDto> {
    let claims = require_client_claims(ctx)?;
    require_manage(claims)?;
    let tenant_id = require_tenant_id(ctx)?;
    let limit = page_limit(input.limit)?;
    let mode = input.mode.trim().to_ascii_uppercase();
    let db = tenant_db(ctx, tenant_id).await?;
    let search = format!("%{}%", input.search.unwrap_or_default());
    let cursor = name_cursor(input.cursor.as_deref())?;

    let statement = population_options_statement(tenant_id, limit, &mode, search, cursor)?;
    let rows = db
        .query_all(statement)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let has_next = rows.len() as u64 > limit;
    let rows = rows.into_iter().take(limit as usize).collect::<Vec<_>>();
    let next_cursor = if has_next {
        rows.last().map(population_option_cursor).transpose()?
    } else {
        None
    };
    let items = rows
        .into_iter()
        .map(population_option)
        .collect::<Result<Vec<_>>>()?;

    Ok(PerformancePopulationOptionsDto { items, next_cursor })
}

fn population_options_statement(
    tenant_id: Uuid,
    limit: u64,
    mode: &str,
    search: String,
    cursor: Option<(String, Uuid)>,
) -> Result<Statement> {
    match (mode, cursor) {
        ("DEPARTMENTS", Some((name, id))) => Ok(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id,name,NULL::uuid AS department_id,NULL::uuid AS location_id FROM department WHERE tenant_id=$1 AND name ILIKE $2 AND (name>$3 OR (name=$3 AND id>$4)) ORDER BY name,id LIMIT $5",
            [tenant_id.into(), search.into(), name.into(), id.into(), (limit + 1).into()],
        )),
        ("DEPARTMENTS", None) => Ok(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id,name,NULL::uuid AS department_id,NULL::uuid AS location_id FROM department WHERE tenant_id=$1 AND name ILIKE $2 ORDER BY name,id LIMIT $3",
            [tenant_id.into(), search.into(), (limit + 1).into()],
        )),
        ("LOCATIONS", Some((name, id))) => Ok(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id,name,NULL::uuid AS department_id,NULL::uuid AS location_id FROM location WHERE tenant_id=$1 AND name ILIKE $2 AND (name>$3 OR (name=$3 AND id>$4)) ORDER BY name,id LIMIT $5",
            [tenant_id.into(), search.into(), name.into(), id.into(), (limit + 1).into()],
        )),
        ("LOCATIONS", None) => Ok(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id,name,NULL::uuid AS department_id,NULL::uuid AS location_id FROM location WHERE tenant_id=$1 AND name ILIKE $2 ORDER BY name,id LIMIT $3",
            [tenant_id.into(), search.into(), (limit + 1).into()],
        )),
        ("EMPLOYEES", Some((name, id))) => Ok(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id,BTRIM(CONCAT(first_name,' ',last_name)) AS name,department_id,location_id FROM employee WHERE tenant_id=$1 AND is_deleted=FALSE AND BTRIM(CONCAT(first_name,' ',last_name)) ILIKE $2 AND (BTRIM(CONCAT(first_name,' ',last_name))>$3 OR (BTRIM(CONCAT(first_name,' ',last_name))=$3 AND id>$4)) ORDER BY name,id LIMIT $5",
            [tenant_id.into(), search.into(), name.into(), id.into(), (limit + 1).into()],
        )),
        ("EMPLOYEES", None) => Ok(Statement::from_sql_and_values(
            DatabaseBackend::Postgres,
            "SELECT id,BTRIM(CONCAT(first_name,' ',last_name)) AS name,department_id,location_id FROM employee WHERE tenant_id=$1 AND is_deleted=FALSE AND BTRIM(CONCAT(first_name,' ',last_name)) ILIKE $2 ORDER BY name,id LIMIT $3",
            [tenant_id.into(), search.into(), (limit + 1).into()],
        )),
        _ => Err(KabiPayError::Validation(
            "Population options mode must be DEPARTMENTS, LOCATIONS, or EMPLOYEES".into(),
        ).into_graphql()),
    }
}

fn population_option_cursor(row: &sea_orm::QueryResult) -> Result<String> {
    let name = row
        .try_get::<String>("", "name")
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    let id = row
        .try_get::<Uuid>("", "id")
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    Ok(format!("{name}|{id}"))
}

fn population_option(
    row: sea_orm::QueryResult,
) -> Result<PerformancePopulationOptionDto> {
    Ok(PerformancePopulationOptionDto {
        id: row
            .try_get::<Uuid>("", "id")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .to_string()
            .into(),
        name: row
            .try_get("", "name")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?,
        department_id: row
            .try_get::<Option<Uuid>>("", "department_id")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .map(|id| id.to_string().into()),
        location_id: row
            .try_get::<Option<Uuid>>("", "location_id")
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .map(|id| id.to_string().into()),
    })
}
