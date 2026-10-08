//! Explicit whole-table reset groups must be closed under FK references and exclude retained identities.
use crate::{options::ImportOptions, preview::ImportPlan, reset::foundational};
use anyhow::{bail, Result};
use sea_orm::{ConnectionTrait, DbBackend, QueryResult, Statement};
use std::collections::HashSet;

pub fn validate_truncate_references(
    schema: &str,
    tables: &[String],
    fks: &[QueryResult],
) -> Result<()> {
    let selected: HashSet<_> = tables.iter().map(String::as_str).collect();
    for table in tables {
        if foundational(table, schema)
            || matches!(table.as_str(), "user" | "employee" | "user_role")
        {
            bail!("RESET_TRUNCATE_PRESERVED_TABLE");
        }
    }
    for fk in fks {
        let parent: String = fk.try_get("", "parent")?;
        let child: String = fk.try_get("", "child")?;
        let child_schema: String = fk.try_get("", "child_schema")?;
        if selected.contains(parent.as_str())
            && (child_schema != schema || !selected.contains(child.as_str()))
        {
            bail!("RESET_TRUNCATE_REFERENCE_OUTSIDE_GROUP");
        }
    }
    Ok(())
}

pub async fn validate_lifecycle<C: ConnectionTrait>(
    db: &C,
    options: &ImportOptions,
    plan: &ImportPlan,
) -> Result<()> {
    let triggers = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT c.relname AS table_name FROM pg_trigger t JOIN pg_class c ON c.oid=t.tgrelid JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND NOT t.tgisinternal AND t.tgenabled<>'D' AND (t.tgtype::int & 8)<>0",
        [options.schema_name.clone().into()])).await?;
    for trigger in triggers {
        let table: String = trigger.try_get("", "table_name")?;
        if options.reset_delete_tables.contains(&table)
            && plan
                .tables
                .iter()
                .any(|state| state.name == table && state.rows > 0)
        {
            bail!("RESET_DELETE_TRIGGER_REQUIRES_REVIEW");
        }
    }
    if !options.reset_truncate_tables.is_empty() {
        let inheritance = db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT c.relname AS table_name FROM pg_class c JOIN pg_namespace n ON n.oid=c.relnamespace WHERE n.nspname=$1 AND EXISTS(SELECT 1 FROM pg_inherits i WHERE i.inhrelid=c.oid OR i.inhparent=c.oid)",
            [options.schema_name.clone().into()])).await?;
        for row in inheritance {
            if options
                .reset_truncate_tables
                .contains(&row.try_get::<String>("", "table_name")?)
            {
                bail!("RESET_TRUNCATE_INHERITANCE_REQUIRES_REVIEW");
            }
        }
    }
    Ok(())
}
