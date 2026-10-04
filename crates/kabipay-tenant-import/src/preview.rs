//! Read-only reviewed state/digests; the apply transaction rechecks after taking write locks.
use crate::{contract::ImportPackage, options::ImportOptions};
use anyhow::{bail, Result};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct PreservedUser {
    pub id: Uuid,
    pub username: String,
    pub employee_id: Option<Uuid>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct TableState {
    pub name: String,
    pub rows: i64,
    pub fingerprint: String,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ImportPlan {
    pub tenant_id: Uuid,
    pub schema_name: String,
    pub mode: String,
    pub package_hash: String,
    pub configuration_hash: String,
    pub state_hash: String,
    pub digest: String,
    pub tables: Vec<TableState>,
    pub preserved_users: Vec<PreservedUser>,
    pub source_rows: usize,
    pub core_ready_rows: usize,
    pub period_ready_rows: usize,
    pub blocking_issues: usize,
    pub schema_fingerprint: String,
    pub reset: Option<crate::reset::ResetManifest>,
    pub source_issues: Vec<crate::report::SourceIssue>,
    #[serde(default)]
    pub actions: Vec<crate::preview_actions::PlannedAction>,
}
pub fn hash(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
pub fn safe_identifier(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 63
        && value
            .bytes()
            .all(|c| c.is_ascii_alphanumeric() || c == b'_')
}

pub async fn preview<C: ConnectionTrait>(
    db: &C,
    package: &ImportPackage,
    options: &ImportOptions,
    package_hash: &str,
    replace: bool,
) -> Result<ImportPlan> {
    options.validate()?;
    let table_names = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT tablename FROM pg_tables WHERE schemaname=$1 ORDER BY tablename",
            [options.schema_name.clone().into()],
        ))
        .await?;
    let mut tables = Vec::new();
    for row in table_names {
        let name: String = row.try_get("", "tablename")?;
        if !safe_identifier(&name) {
            bail!("TABLE_IDENTIFIER_UNSUPPORTED");
        }
        let state=db.query_one(Statement::from_string(DbBackend::Postgres,
            format!("SELECT COUNT(*) AS n,md5(COALESCE(string_agg(md5(to_jsonb(t)::text),'' ORDER BY md5(to_jsonb(t)::text)),'')) AS fingerprint FROM \"{}\".\"{name}\" t",options.schema_name))).await?
            .ok_or_else(||anyhow::anyhow!("TABLE_STATE_UNRESOLVED"))?;
        tables.push(TableState {
            name,
            rows: state.try_get("", "n")?,
            fingerprint: state.try_get("", "fingerprint")?,
        });
    }
    let mut users = Vec::new();
    for username in &options.preserved_usernames {
        let matches=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT u.id,u.username,e.id AS employee_id FROM \"user\" u LEFT JOIN employee e ON e.user_id=u.id AND e.tenant_id=u.tenant_id AND NOT e.is_deleted WHERE u.tenant_id=$1 AND lower(u.username)=lower($2) AND NOT u.is_deleted",
            [options.tenant_id.into(),username.clone().into()])).await?;
        if matches.len() != 1 {
            bail!("PRESERVED_ACCOUNT_UNRESOLVED");
        }
        let row = &matches[0];
        users.push(PreservedUser {
            id: row.try_get("", "id")?,
            username: row.try_get("", "username")?,
            employee_id: row.try_get("", "employee_id")?,
        });
    }
    if replace && !users.iter().any(|user| user.id == options.actor_id) {
        bail!("REPLACE_ACTOR_MUST_BE_PRESERVED");
    }
    for username in &options.reviewed_absent_usernames {
        let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT COUNT(*) AS n FROM \"user\" WHERE tenant_id=$1 AND lower(username)=lower($2)",[options.tenant_id.into(),username.clone().into()])).await?
            .ok_or_else(||anyhow::anyhow!("ACCOUNT_STATE_UNRESOLVED"))?;
        if row.try_get::<i64>("", "n")? > 0 {
            bail!("REVIEWED_ABSENT_ACCOUNT_EXISTS");
        }
    }
    let mode = if replace { "REPLACE" } else { "IMPORT" };
    let configuration_hash = hash(&serde_json::to_vec(options)?);
    let schema_state=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT md5(COALESCE((SELECT string_agg(table_name||':'||column_name||':'||data_type||':'||is_nullable||':'||COALESCE(column_default,''),',' ORDER BY table_name,ordinal_position) FROM information_schema.columns WHERE table_schema=$1),''))||md5(COALESCE((SELECT string_agg(pg_get_constraintdef(c.oid),',' ORDER BY c.conname,c.oid) FROM pg_constraint c JOIN pg_class t ON t.oid=c.conrelid JOIN pg_namespace n ON n.oid=t.relnamespace WHERE n.nspname=$1),'')) AS fingerprint",
        [options.schema_name.clone().into()])).await?.ok_or_else(||anyhow::anyhow!("SCHEMA_STATE_UNRESOLVED"))?;
    let schema_fingerprint: String = schema_state.try_get("", "fingerprint")?;
    let state_hash = hash(&serde_json::to_vec(&(&tables, &schema_fingerprint))?);
    let core_ready_rows = package
        .employees
        .iter()
        .filter(|row| {
            row.core_ready().is_ok()
                && !package.issues.iter().any(|issue| {
                    issue.severity == "BLOCK_EMPLOYEE"
                        && issue.source_ref.as_ref().is_some_and(|source| {
                            source.sheet == row.source_ref.sheet && source.row == row.source_ref.row
                        })
                })
        })
        .count();
    let period_ready_rows = package
        .employees
        .iter()
        .filter_map(|r| r.period_input.as_ref())
        .filter(|p| {
            p.ready && kabipay_payroll::services::payroll_rules::calculate_period(p).is_ok()
        })
        .count();
    let blocking_issues = package
        .issues
        .iter()
        .filter(|issue| issue.severity == "BLOCK_TENANT" || issue.severity == "BLOCK_EMPLOYEE")
        .count();
    let source_issues = package
        .issues
        .iter()
        .map(|issue| crate::report::SourceIssue {
            source_ref: issue.source_ref.clone(),
            severity: issue.severity.clone(),
            section: issue.section.clone(),
            code: issue.code.clone(),
            field: issue.field.clone(),
        })
        .collect();
    let actions = crate::preview_actions::actions(db, package, options, &users, replace).await?;
    let mut plan = ImportPlan {
        tenant_id: options.tenant_id,
        schema_name: options.schema_name.clone(),
        mode: mode.into(),
        package_hash: package_hash.into(),
        configuration_hash,
        state_hash,
        digest: String::new(),
        tables,
        preserved_users: users,
        source_rows: package.employees.len(),
        core_ready_rows,
        period_ready_rows,
        blocking_issues,
        schema_fingerprint,
        reset: None,
        source_issues,
        actions,
    };
    if replace {
        plan.reset = Some(crate::reset::prepare(db, options, &plan).await?);
    }
    plan.digest = hash(&serde_json::to_vec(&(
        options.tenant_id,
        &options.schema_name,
        mode,
        package_hash,
        &plan.configuration_hash,
        &plan.state_hash,
        &plan.preserved_users,
        &plan.reset,
    ))?);
    Ok(plan)
}

pub async fn lock_tables<C: ConnectionTrait>(
    db: &C,
    schema: &str,
    tables: &[TableState],
) -> Result<()> {
    if !crate::options::valid_schema(schema) {
        bail!("TARGET_CONFIGURATION_INVALID");
    }
    for table in tables {
        if !safe_identifier(&table.name) {
            bail!("TABLE_IDENTIFIER_UNSUPPORTED");
        }
        db.execute(Statement::from_string(
            DbBackend::Postgres,
            format!(
                "LOCK TABLE \"{schema}\".\"{}\" IN SHARE ROW EXCLUSIVE MODE",
                table.name
            ),
        ))
        .await?;
    }
    Ok(())
}
