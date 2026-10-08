//! Resolve source labels only within this tenant, without inventing a department.
use crate::contract::ImportEmployee;
use anyhow::{bail, Result};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;
pub async fn department<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    row: &ImportEmployee,
) -> Result<&'static str> {
    let Some(name) = row.employee.department.as_deref() else {
        return Ok("UNCHANGED");
    };
    validate_label(name)?;
    let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,is_deleted FROM department WHERE tenant_id=$1 AND lower(btrim(name))=lower(btrim($2))",[tenant.into(),name.into()])).await?;
    if rows.len() > 1
        || rows
            .first()
            .is_some_and(|row| row.try_get::<bool>("", "is_deleted").unwrap_or(true))
    {
        bail!("DEPARTMENT_IDENTITY_AMBIGUOUS");
    }
    let id = if let Some(row) = rows.first() {
        row.try_get::<Uuid>("", "id")?
    } else {
        let id = Uuid::new_v4();
        let hash = crate::preview::hash(name.trim().to_lowercase().as_bytes());
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO department(id,tenant_id,name,code) VALUES($1,$2,$3,$4)",
            [
                id.into(),
                tenant.into(),
                name.into(),
                format!("IMPORT_{}", &hash[..20]).into(),
            ],
        ))
        .await?;
        id
    };
    let result=db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE employee SET department_id=$3,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND department_id IS DISTINCT FROM $3",[tenant.into(),employee.into(),id.into()])).await?;
    Ok(if result.rows_affected() > 0 {
        "UPDATED"
    } else {
        "UNCHANGED"
    })
}
pub async fn designation<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    row: &ImportEmployee,
) -> Result<&'static str> {
    let Some(title) = row.employee.designation.as_deref() else {
        return Ok("UNCHANGED");
    };
    validate_label(title)?;
    let employee_row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT department_id FROM employee WHERE tenant_id=$1 AND id=$2",
            [tenant.into(), employee.into()],
        ))
        .await?
        .ok_or_else(|| anyhow::anyhow!("EMPLOYEE_UNRESOLVED"))?;
    let department: Option<Uuid> = employee_row.try_get("", "department_id")?;
    let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,department_id,is_deleted FROM designation WHERE tenant_id=$1 AND lower(btrim(title))=lower(btrim($2)) AND ($3::uuid IS NULL OR department_id=$3)",
        [tenant.into(),title.into(),department.into()])).await?;
    if rows.len() > 1
        || rows
            .first()
            .is_some_and(|row| row.try_get::<bool>("", "is_deleted").unwrap_or(true))
    {
        bail!("DESIGNATION_IDENTITY_AMBIGUOUS");
    }
    let (id, department) = if let Some(row) = rows.first() {
        (
            row.try_get::<Uuid>("", "id")?,
            row.try_get::<Uuid>("", "department_id")?,
        )
    } else {
        let department =
            department.ok_or_else(|| anyhow::anyhow!("DESIGNATION_DEPARTMENT_REQUIRED"))?;
        let id = Uuid::new_v4();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO designation(id,tenant_id,department_id,title) VALUES($1,$2,$3,$4)",
            [id.into(), tenant.into(), department.into(), title.into()],
        ))
        .await?;
        (id, department)
    };
    let result=db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE employee SET designation_id=$3,department_id=$4,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND (designation_id IS DISTINCT FROM $3 OR department_id IS DISTINCT FROM $4)",
        [tenant.into(),employee.into(),id.into(),department.into()])).await?;
    Ok(if result.rows_affected() > 0 {
        "UPDATED"
    } else {
        "UNCHANGED"
    })
}
fn validate_label(value: &str) -> Result<()> {
    if value.trim().is_empty() || value.len() > 255 || value.chars().any(char::is_control) {
        bail!("ORGANIZATION_LABEL_INVALID");
    }
    Ok(())
}
