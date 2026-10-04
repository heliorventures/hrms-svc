//! Employee/core and independent profile writes. No name-derived login or joining date.
use crate::{contract::ImportEmployee, options::ImportOptions};
use anyhow::{bail, Result};
use chrono::Utc;
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_employee::services::employee_service::{self, NewEmployee};
use sea_orm::{
    ActiveModelTrait, ConnectionTrait, DatabaseTransaction, DbBackend, EntityTrait, Set, Statement,
    Value,
};
use serde::Serialize;
use uuid::Uuid;

#[derive(Serialize)]
pub struct NewCredential {
    pub employee_code: String,
    pub username: String,
    pub temporary_password: String,
}

pub async fn core(
    txn: &DatabaseTransaction,
    row: &ImportEmployee,
    options: &ImportOptions,
) -> Result<(Uuid, &'static str)> {
    row.core_ready()?;
    let data = &row.employee;
    let code = data
        .code
        .as_deref()
        .ok_or_else(|| anyhow::anyhow!("EMPLOYEE_CODE_REQUIRED"))?;
    let found = employee::Entity::find()
        .from_raw_sql(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM employee WHERE tenant_id=$1 AND lower(employee_code)=lower($2)",
            [options.tenant_id.into(), code.into()],
        ))
        .all(txn)
        .await?;
    if found.len() > 1 {
        bail!("EMPLOYEE_IDENTITY_AMBIGUOUS");
    }
    let first = data
        .first_name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("FIRST_NAME_REQUIRED"))?;
    let last = data
        .last_name
        .clone()
        .ok_or_else(|| anyhow::anyhow!("LAST_NAME_REQUIRED"))?;
    let joining = data
        .joining_date
        .ok_or_else(|| anyhow::anyhow!("JOINING_DATE_REQUIRED"))?;
    let (id, outcome) = if let Some(existing) = found.into_iter().next() {
        if existing.is_deleted {
            bail!("DELETED_EMPLOYEE_REQUIRES_REVIEW");
        }
        let id = existing.id;
        if existing.first_name == first
            && existing.last_name == last
            && existing.date_of_joining == joining
        {
            (id, "UNCHANGED")
        } else {
            let mut active: employee::ActiveModel = existing.into();
            active.first_name = Set(first);
            active.last_name = Set(last);
            active.date_of_joining = Set(joining);
            active.updated_at = Set(Utc::now());
            active.update(txn).await?;
            (id, "UPDATED")
        }
    } else {
        let new = NewEmployee {
            employee_code: code.into(),
            first_name: first,
            last_name: last,
            date_of_joining: joining,
            department_id: None,
            designation_id: None,
            reporting_manager_id: None,
            employment_type: None,
            status: "ACTIVE".into(),
            user_id: None,
        };
        let created = employee_service::create(txn, options.tenant_id, new).await?;
        (created.id, "CREATED")
    };
    Ok((id, outcome))
}

pub async fn optional_profile<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
    row: &ImportEmployee,
) -> Result<&'static str> {
    let mut changed = false;
    let data = &row.employee;
    if data
        .uan
        .as_ref()
        .is_some_and(|value| value.len() != 12 || !value.bytes().all(|b| b.is_ascii_digit()))
        || data
            .gender
            .as_ref()
            .is_some_and(|value| !matches!(value.as_str(), "MALE" | "FEMALE" | "OTHER"))
    {
        bail!("OPTIONAL_PROFILE_FORMAT_INVALID");
    }
    for (field, column, value) in [
        (
            "birth_date",
            "date_of_birth",
            data.birth_date.map(Value::from),
        ),
        (
            "confirmation_date",
            "confirmation_date",
            data.confirmation_date.map(Value::from),
        ),
        (
            "exit_date",
            "imported_exit_date",
            data.exit_date.map(Value::from),
        ),
        (
            "last_working_date",
            "imported_last_working_date",
            data.last_working_date.map(Value::from),
        ),
        ("gender", "gender", data.gender.clone().map(Value::from)),
        ("uan", "uan_number", data.uan.clone().map(Value::from)),
    ] {
        let clear = row.clear_fields.iter().any(|item| item == field);
        if value.is_some() || clear {
            let value = value.unwrap_or_else(|| {
                if field.ends_with("date") {
                    Value::ChronoDate(None)
                } else {
                    Value::String(None)
                }
            });
            let result=db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                format!("UPDATE employee SET {column}=$3,updated_at=NOW() WHERE tenant_id=$1 AND id=$2 AND {column} IS DISTINCT FROM $3"),[tenant.into(),id.into(),value])).await?;
            changed |= result.rows_affected() > 0;
        }
    }
    Ok(if changed { "UPDATED" } else { "UNCHANGED" })
}

pub async fn identity<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
    row: &ImportEmployee,
) -> Result<&'static str> {
    let mut outcome = "UNCHANGED";
    let empty = crate::contract::Identity {
        pan: None,
        aadhaar_last_four: None,
        verified: false,
    };
    let identity = row.identity.as_ref().unwrap_or(&empty);
    if identity.verified {
        bail!("IMPORT_CANNOT_VERIFY_IDENTITY");
    }
    for (field, table, column, value) in [
        ("pan", "employee_pan", "pan_number", identity.pan.as_deref()),
        (
            "aadhaar_last_four",
            "employee_aadhaar",
            "aadhaar_last4",
            identity.aadhaar_last_four.as_deref(),
        ),
    ] {
        let clear = row.clear_fields.iter().any(|item| item == field);
        if clear {
            let result = db
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    format!(
                        "DELETE FROM {table} WHERE tenant_id=$1 AND employee_id=$2 AND is_primary"
                    ),
                    [tenant.into(), id.into()],
                ))
                .await?;
            if result.rows_affected() > 0 {
                outcome = "UPDATED";
            }
            continue;
        }
        let Some(value) = value else { continue };
        if (field == "aadhaar_last_four"
            && (value.len() != 4 || !value.bytes().all(|c| c.is_ascii_digit())))
            || (field == "pan"
                && (value.len() != 10
                    || !value.bytes().take(5).all(|c| c.is_ascii_uppercase())
                    || !value.bytes().skip(5).take(4).all(|c| c.is_ascii_digit())
                    || !value.as_bytes()[9].is_ascii_uppercase()))
        {
            bail!("IDENTITY_FORMAT_INVALID");
        }
        let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            format!("SELECT id,{column} AS value FROM {table} WHERE tenant_id=$1 AND employee_id=$2 AND is_primary"),[tenant.into(),id.into()])).await?;
        if rows.len() > 1 {
            bail!("PRIMARY_IDENTITY_AMBIGUOUS");
        }
        if let Some(existing) = rows.first() {
            if existing.try_get::<String>("", "value")? != value {
                db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                    format!("UPDATE {table} SET {column}=$3,is_verified=FALSE,verified_at=NULL,updated_at=NOW() WHERE tenant_id=$1 AND id=$2"),
                    [tenant.into(),existing.try_get::<Uuid>("","id")?.into(),value.into()])).await?;
                outcome = "UPDATED";
            }
        } else {
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                format!("INSERT INTO {table}(id,tenant_id,employee_id,{column},is_primary,is_verified) VALUES($1,$2,$3,$4,TRUE,FALSE)"),
                [Uuid::new_v4().into(),tenant.into(),id.into(),value.into()])).await?;
            outcome = "CREATED";
        }
    }
    Ok(outcome)
}

pub async fn bank<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    id: Uuid,
    row: &ImportEmployee,
) -> Result<&'static str> {
    let mut outcome = "UNCHANGED";
    let Some(bank) = &row.bank else {
        for (field, column) in [
            ("bank.account_holder", "account_holder"),
            ("bank.branch", "branch_name"),
            ("bank.account_type", "account_type"),
        ] {
            if row.clear_fields.iter().any(|value| value == field) {
                let result=db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                format!("UPDATE employee_bank SET {column}=NULL,updated_at=NOW() WHERE tenant_id=$1 AND employee_id=$2 AND is_primary AND {column} IS NOT NULL"),[tenant.into(),id.into()])).await?;
                if result.rows_affected() > 0 {
                    outcome = "UPDATED";
                }
            }
        }
        return Ok(outcome);
    };
    if bank.verified
        || bank.account_number.is_empty()
        || bank.account_number.len() > 34
        || !bank.account_number.bytes().all(|c| c.is_ascii_digit())
        || bank.ifsc.len() != 11
        || !bank.ifsc.bytes().take(4).all(|c| c.is_ascii_uppercase())
        || bank.ifsc.as_bytes()[4] != b'0'
        || !bank.ifsc.bytes().skip(5).all(|c| c.is_ascii_alphanumeric())
        || bank.bank_name.trim().is_empty()
    {
        bail!("BANK_FORMAT_INVALID");
    }
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT * FROM employee_bank WHERE tenant_id=$1 AND employee_id=$2 AND is_primary",
            [tenant.into(), id.into()],
        ))
        .await?;
    if rows.len() > 1 {
        bail!("PRIMARY_BANK_AMBIGUOUS");
    }
    let clears = |field: &str| row.clear_fields.iter().any(|item| item == field);
    if let Some(existing) = rows.first() {
        let target_optional =
            |field: &str, column: &str, provided: &Option<String>| -> Result<Option<String>> {
                if clears(field) {
                    Ok(None)
                } else {
                    Ok(provided
                        .clone()
                        .or(existing.try_get::<Option<String>>("", column)?))
                }
            };
        let same = existing.try_get::<String>("", "account_number")? == bank.account_number
            && existing.try_get::<String>("", "ifsc_code")? == bank.ifsc
            && existing.try_get::<String>("", "bank_name")? == bank.bank_name
            && existing.try_get::<Option<String>>("", "account_type")?
                == target_optional("bank.account_type", "account_type", &bank.account_type)?
            && existing.try_get::<Option<String>>("", "account_holder")?
                == target_optional(
                    "bank.account_holder",
                    "account_holder",
                    &bank.account_holder,
                )?
            && existing.try_get::<Option<String>>("", "branch_name")?
                == target_optional("bank.branch", "branch_name", &bank.branch)?;
        if same {
            return Ok("UNCHANGED");
        }
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE employee_bank SET is_verified=CASE WHEN account_number=$3 AND ifsc_code=$4 THEN is_verified ELSE FALSE END,account_number=$3,ifsc_code=$4,bank_name=$5,account_type=CASE WHEN $9 THEN NULL ELSE COALESCE($6,account_type) END,account_holder=CASE WHEN $10 THEN NULL ELSE COALESCE($7,account_holder) END,branch_name=CASE WHEN $11 THEN NULL ELSE COALESCE($8,branch_name) END,updated_at=NOW() WHERE tenant_id=$1 AND id=$2",
            [tenant.into(),existing.try_get::<Uuid>("","id")?.into(),bank.account_number.clone().into(),bank.ifsc.clone().into(),bank.bank_name.clone().into(),
                bank.account_type.clone().into(),bank.account_holder.clone().into(),bank.branch.clone().into(),clears("bank.account_type").into(),clears("bank.account_holder").into(),clears("bank.branch").into()])).await?;
        outcome = "UPDATED";
    } else {
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO employee_bank(id,tenant_id,employee_id,account_number,ifsc_code,bank_name,account_type,account_holder,branch_name,is_primary,is_verified) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,TRUE,FALSE)",
            [Uuid::new_v4().into(),tenant.into(),id.into(),bank.account_number.clone().into(),bank.ifsc.clone().into(),bank.bank_name.clone().into(),
                bank.account_type.clone().into(),bank.account_holder.clone().into(),bank.branch.clone().into()])).await?;
        outcome = "CREATED";
    }
    Ok(outcome)
}
