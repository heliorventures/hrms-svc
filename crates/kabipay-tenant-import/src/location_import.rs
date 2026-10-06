//! Reviewed initial assignments. Existing dated history is never rewritten by an import.
use anyhow::{bail, Result};
use chrono::NaiveDate;
use kabipay_common::working_calendar::{audit_calendar, bump_calendar_revision};
use sea_orm::{ConnectionTrait, DatabaseTransaction, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LocationInput {
    pub name: String,
    pub effective_from: NaiveDate,
}

impl LocationInput {
    pub fn normalized_name(&self) -> Result<String> {
        if self.name.chars().any(char::is_control) {
            bail!("LOCATION_NAME_INVALID");
        }
        kabipay_employee::services::company_location_service::normalized_name(&self.name)
            .map_err(|_| anyhow::anyhow!("LOCATION_NAME_INVALID"))
    }
}

#[derive(Clone, Debug)]
pub struct AssignmentState {
    pub current: Option<Uuid>,
    pub latest: Option<(Option<Uuid>, NaiveDate)>,
    pub joining: NaiveDate,
    pub calendar_active: bool,
}

/// True means insert one initial version; false is an exact idempotent replay.
pub fn needs_assignment(
    state: &AssignmentState,
    desired: Uuid,
    effective: NaiveDate,
    today: NaiveDate,
) -> Result<bool> {
    if effective < state.joining || effective > today {
        bail!("LOCATION_EFFECTIVE_DATE_INVALID");
    }
    if let Some((location, date)) = state.latest {
        if location == Some(desired) && state.current == Some(desired) && date == effective {
            return Ok(false);
        }
        bail!("LOCATION_ASSIGNMENT_HISTORY_REVIEW_REQUIRED");
    }
    if state.current.is_some_and(|current| current != desired) {
        bail!("LOCATION_EXISTING_ASSIGNMENT_REVIEW_REQUIRED");
    }
    if state.calendar_active && effective != today {
        bail!("LOCATION_ACTIVE_CALENDAR_REQUIRES_CURRENT_DATE");
    }
    Ok(true)
}

/// Caller holds the calendar lock before import table locks, and owns the section savepoint.
pub async fn save(
    db: &DatabaseTransaction,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    input: &LocationInput,
    today: NaiveDate,
) -> Result<&'static str> {
    let name = input.normalized_name()?;
    let employee_row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT location_id,date_of_joining FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted FOR UPDATE",
        [tenant.into(), employee.into()])).await?
        .ok_or_else(|| anyhow::anyhow!("LOCATION_EMPLOYEE_UNRESOLVED"))?;
    let profile = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT activation_date FROM working_calendar_profile WHERE tenant_id=$1",
            [tenant.into()],
        ))
        .await?;
    let latest = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT location_id,effective_from FROM employee_location_assignment WHERE tenant_id=$1 AND employee_id=$2 ORDER BY effective_from DESC LIMIT 1",
        [tenant.into(), employee.into()])).await?;
    let state = AssignmentState {
        current: employee_row.try_get("", "location_id")?,
        joining: employee_row.try_get("", "date_of_joining")?,
        calendar_active: profile.is_some(),
        latest: latest
            .map(|row| {
                Ok::<_, sea_orm::DbErr>((
                    row.try_get("", "location_id")?,
                    row.try_get("", "effective_from")?,
                ))
            })
            .transpose()?,
    };
    let (location, create) = resolve_location(db, tenant, &name).await?;
    if !needs_assignment(&state, location, input.effective_from, today)? {
        return Ok("UNCHANGED");
    }
    if create {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO location(id,tenant_id,name) VALUES($1,$2,$3)",
            [location.into(), tenant.into(), name.clone().into()],
        ))
        .await?;
        audit_calendar(
            db,
            tenant,
            actor,
            "location",
            location,
            "IMPORT_CREATE",
            None,
            Some(serde_json::json!({"name":name})),
        )
        .await?;
    }
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_location_assignment(id,tenant_id,employee_id,location_id,effective_from,revision,changed_by) VALUES($1,$2,$3,$4,$5,1,$6)",
        [Uuid::new_v4().into(), tenant.into(), employee.into(), location.into(), input.effective_from.into(), actor.into()])).await?;
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE employee SET location_id=$3,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND location_id IS DISTINCT FROM $3",
        [tenant.into(), employee.into(), location.into()])).await?;
    bump_calendar_revision(db, tenant).await?;
    audit_calendar(db, tenant, actor, "employee_location", employee, "IMPORT_ASSIGN",
        Some(serde_json::json!({"locationId":state.current})),
        Some(serde_json::json!({"locationId":location,"effectiveFrom":input.effective_from,"revision":1}))).await?;
    Ok("CREATED")
}

async fn resolve_location(
    db: &DatabaseTransaction,
    tenant: Uuid,
    name: &str,
) -> Result<(Uuid, bool)> {
    let rows = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,is_deleted FROM location WHERE tenant_id=$1 AND lower(regexp_replace(btrim(name),'\\s+',' ','g'))=lower($2)",
        [tenant.into(), name.into()])).await?;
    let mut active = Vec::new();
    for row in &rows {
        if !row.try_get::<bool>("", "is_deleted")? {
            active.push(row.try_get::<Uuid>("", "id")?);
        }
    }
    match active.as_slice() {
        [id] => Ok((*id, false)),
        [] if rows.is_empty() => Ok((Uuid::new_v4(), true)),
        [] => bail!("LOCATION_RETIRED_REVIEW_REQUIRED"),
        _ => bail!("LOCATION_IDENTITY_AMBIGUOUS"),
    }
}
