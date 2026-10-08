//! Explicit reset scope and dependency order; never CASCADE or another schema.
use crate::{
    options::ImportOptions,
    preview::{safe_identifier, ImportPlan},
};
use anyhow::{bail, Result};
use sea_orm::{ConnectionTrait, DbBackend, Statement, Value};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeSet, HashSet};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ResetManifest {
    pub delete_order: Vec<String>,
    #[serde(default)]
    pub truncate_tables: Vec<String>,
    #[serde(default)]
    pub backup: crate::backup::BackupPolicy,
    pub retained_tables: Vec<String>,
    pub preserved_user_ids: Vec<Uuid>,
    pub preserved_employee_ids: Vec<Uuid>,
    pub clear_nullable_links: Vec<ClearLink>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ClearLink {
    pub child: String,
    pub parent: String,
    pub child_columns: Vec<String>,
    pub parent_columns: Vec<String>,
    pub clear_columns: Vec<String>,
    pub rows: i64,
}

pub fn foundational(table: &str, schema: &str) -> bool {
    matches!(
        table,
        "role"
            | "permission"
            | "role_permission"
            | "permission_scope"
            | "tenant_import_run"
            | "tenant_import_record"
    ) || table == format!("{schema}_databasechangelog")
        || table == format!("{schema}_databasechangeloglock")
        || matches!(table, "databasechangelog" | "databasechangeloglock")
}
pub fn deletion_order(tables: &[String], dependencies: &[(String, String)]) -> Result<Vec<String>> {
    let mut pending: BTreeSet<_> = tables.iter().cloned().collect();
    let mut output = Vec::new();
    while !pending.is_empty() {
        let next = pending
            .iter()
            .find(|candidate| {
                !dependencies.iter().any(|(child, parent)| {
                    child != parent && parent == *candidate && pending.contains(child)
                })
            })
            .cloned();
        let Some(next) = next else {
            bail!("RESET_FOREIGN_KEY_CYCLE_REQUIRES_REVIEW");
        };
        pending.remove(&next);
        output.push(next);
    }
    Ok(output)
}
fn selector(table: &str) -> &'static str {
    match table {
        "user" => "NOT(id=ANY($1::uuid[]))",
        "employee" => "NOT(id=ANY($2::uuid[]))",
        "user_role" => "NOT(user_id=ANY($1::uuid[]))",
        _ => "TRUE",
    }
}
fn qualified_selector(table: &str, alias: &str) -> String {
    match table {
        "user" => format!("NOT({alias}.id=ANY($1::uuid[]))"),
        "employee" => format!("NOT({alias}.id=ANY($2::uuid[]))"),
        "user_role" => format!("NOT({alias}.user_id=ANY($1::uuid[]))"),
        _ => "TRUE".into(),
    }
}
fn arrays(users: &[Uuid], employees: &[Uuid]) -> Vec<Value> {
    vec![
        Value::Array(
            sea_orm::sea_query::ArrayType::Uuid,
            Some(Box::new(users.iter().map(|id| (*id).into()).collect())),
        ),
        Value::Array(
            sea_orm::sea_query::ArrayType::Uuid,
            Some(Box::new(employees.iter().map(|id| (*id).into()).collect())),
        ),
    ]
}
pub async fn prepare<C: ConnectionTrait>(
    db: &C,
    options: &ImportOptions,
    plan: &ImportPlan,
) -> Result<ResetManifest> {
    let deletes: &[String] = &options.reset_delete_tables;
    if deletes.is_empty()
        || ["user", "employee", "user_role", "user_session"]
            .iter()
            .any(|name| !deletes.iter().any(|t| t == name))
    {
        bail!("RESET_EXPLICIT_TABLE_MANIFEST_REQUIRED");
    }
    let names: HashSet<_> = plan.tables.iter().map(|t| t.name.as_str()).collect();
    let delete_set: HashSet<_> = deletes.iter().map(String::as_str).collect();
    let truncate_set: HashSet<_> = options
        .reset_truncate_tables
        .iter()
        .map(String::as_str)
        .collect();
    let reset_set: HashSet<_> = delete_set.union(&truncate_set).copied().collect();
    let retain_set: HashSet<_> = options
        .reset_retain_tables
        .iter()
        .map(String::as_str)
        .collect();
    if delete_set.len() != deletes.len()
        || truncate_set.len() != options.reset_truncate_tables.len()
        || retain_set.len() != options.reset_retain_tables.len()
        || !reset_set.is_disjoint(&retain_set)
        || !delete_set.is_disjoint(&truncate_set)
        || deletes
            .iter()
            .chain(&options.reset_truncate_tables)
            .any(|name| {
                !safe_identifier(name)
                    || !names.contains(name.as_str())
                    || foundational(name, &options.schema_name)
            })
        || retain_set.iter().any(|name| !names.contains(name))
    {
        bail!("RESET_TABLE_MANIFEST_INVALID");
    }
    if names.iter().any(|name| {
        !reset_set.contains(name)
            && !retain_set.contains(name)
            && !foundational(name, &options.schema_name)
    }) {
        bail!("RESET_UNCLASSIFIED_TABLE");
    }
    let users: Vec<_> = plan.preserved_users.iter().map(|u| u.id).collect();
    let employees: Vec<_> = plan
        .preserved_users
        .iter()
        .filter_map(|u| u.employee_id)
        .collect();
    let fks=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT child.relname AS child,parent.relname AS parent,cn.nspname AS child_schema,pn.nspname AS parent_schema FROM pg_constraint f JOIN pg_class child ON child.oid=f.conrelid JOIN pg_namespace cn ON cn.oid=child.relnamespace JOIN pg_class parent ON parent.oid=f.confrelid JOIN pg_namespace pn ON pn.oid=parent.relnamespace WHERE f.contype='f' AND pn.nspname=$1",
        [options.schema_name.clone().into()])).await?;
    let mut dependencies = Vec::new();
    crate::reset_scope::validate_truncate_references(
        &options.schema_name,
        &options.reset_truncate_tables,
        &fks,
    )?;
    crate::reset_scope::validate_lifecycle(db, options, plan).await?;
    for fk in &fks {
        if fk.try_get::<String>("", "child_schema")? != options.schema_name {
            bail!("RESET_EXTERNAL_FOREIGN_KEY");
        }
    }
    let relations=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT child.relname AS child,parent.relname AS parent,f.confmatchtype::text AS match_type,string_agg(ca.attname,',' ORDER BY k.ordinality) AS child_columns,string_agg(pa.attname,',' ORDER BY k.ordinality) AS parent_columns,string_agg(ca.attname,',' ORDER BY k.ordinality) FILTER(WHERE NOT ca.attnotnull AND ca.attname<>'tenant_id' AND NOT EXISTS(SELECT 1 FROM pg_constraint ck WHERE ck.conrelid=ca.attrelid AND ck.contype='c' AND ca.attnum=ANY(ck.conkey))) AS nullable_columns FROM pg_constraint f JOIN pg_class child ON child.oid=f.conrelid JOIN pg_namespace cn ON cn.oid=child.relnamespace JOIN pg_class parent ON parent.oid=f.confrelid JOIN pg_namespace pn ON pn.oid=parent.relnamespace CROSS JOIN LATERAL unnest(f.conkey,f.confkey) WITH ORDINALITY k(child_att,parent_att,ordinality) JOIN pg_attribute ca ON ca.attrelid=child.oid AND ca.attnum=k.child_att JOIN pg_attribute pa ON pa.attrelid=parent.oid AND pa.attnum=k.parent_att WHERE f.contype='f' AND cn.nspname=$1 AND pn.nspname=$1 GROUP BY f.oid,child.relname,parent.relname ORDER BY child.relname,parent.relname,f.oid",
        [options.schema_name.clone().into()])).await?;
    for relation in relations {
        let child: String = relation.try_get("", "child")?;
        let parent: String = relation.try_get("", "parent")?;
        if !delete_set.contains(parent.as_str()) || truncate_set.contains(child.as_str()) {
            continue;
        }
        // Apply repeats the complete table fingerprint review under locks before using this plan.
        if plan
            .tables
            .iter()
            .any(|table| (table.name == child || table.name == parent) && table.rows == 0)
        {
            continue;
        }
        let child_columns: String = relation.try_get("", "child_columns")?;
        let parent_columns: String = relation.try_get("", "parent_columns")?;
        let mut predicates = Vec::new();
        for (left, right) in child_columns.split(',').zip(parent_columns.split(',')) {
            if !safe_identifier(left) || !safe_identifier(right) {
                bail!("RESET_FOREIGN_KEY_UNSUPPORTED");
            }
            predicates.push(format!("c.\"{left}\"=p.\"{right}\""));
        }
        let retained_child = if delete_set.contains(child.as_str()) {
            format!("NOT({})", qualified_selector(&child, "c"))
        } else {
            "TRUE".into()
        };
        let sql=format!("SELECT COUNT(*) AS n FROM \"{}\".\"{child}\" c JOIN \"{}\".\"{parent}\" p ON {} WHERE {retained_child} AND {} AND cardinality($1::uuid[])>=0 AND cardinality($2::uuid[])>=0",options.schema_name,options.schema_name,predicates.join(" AND "),qualified_selector(&parent,"p"));
        if !delete_set.contains(child.as_str())
            || matches!(child.as_str(), "user" | "employee" | "user_role")
        {
            let count = db
                .query_one(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    sql,
                    arrays(&users, &employees),
                ))
                .await?
                .ok_or_else(|| anyhow::anyhow!("RESET_DEPENDENCIES_UNRESOLVED"))?;
            if count.try_get::<i64>("", "n")? > 0 {
                bail!("RESET_RETAINED_ROW_REFERENCES_DELETED_DATA");
            }
        }
        if !delete_set.contains(child.as_str()) {
            continue;
        }
        let sql=format!("SELECT COUNT(*) AS n FROM \"{}\".\"{child}\" c JOIN \"{}\".\"{parent}\" p ON {} WHERE {} AND {} AND cardinality($1::uuid[])>=0 AND cardinality($2::uuid[])>=0",options.schema_name,options.schema_name,predicates.join(" AND "),qualified_selector(&child,"c"),qualified_selector(&parent,"p"));
        let count = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                arrays(&users, &employees),
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("RESET_DEPENDENCIES_UNRESOLVED"))?
            .try_get::<i64>("", "n")?;
        if count == 0 {
            continue;
        }
        let nullable: Option<String> = relation.try_get("", "nullable_columns")?;
        let clear_columns: Vec<String> = nullable
            .map(|v| v.split(',').map(String::from).collect())
            .unwrap_or_default();
        let simple = relation.try_get::<String>("", "match_type")? == "s";
        let clear = if !clear_columns.is_empty()
            && (simple || clear_columns.len() == child_columns.split(',').count())
        {
            Some(ClearLink {
                child: child.clone(),
                parent: parent.clone(),
                child_columns: child_columns.split(',').map(String::from).collect(),
                parent_columns: parent_columns.split(',').map(String::from).collect(),
                clear_columns,
                rows: count,
            })
        } else {
            None
        };
        dependencies.push(crate::reset_order::Dependency {
            child,
            parent,
            clear,
        });
    }
    // Refuse a schema containing another tenant's rows even though it has a tenant-like name.
    let owned=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT table_name FROM information_schema.columns WHERE table_schema=$1 AND column_name='tenant_id'",[options.schema_name.clone().into()])).await?;
    for row in owned {
        let table: String = row.try_get("", "table_name")?;
        if !safe_identifier(&table) {
            bail!("TABLE_IDENTIFIER_UNSUPPORTED");
        }
        if plan
            .tables
            .iter()
            .any(|state| state.name == table && state.rows == 0)
        {
            continue;
        }
        let result = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!(
                    "SELECT COUNT(*) AS n FROM \"{}\".\"{table}\" WHERE tenant_id<>$1",
                    options.schema_name
                ),
                [options.tenant_id.into()],
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("RESET_OWNERSHIP_UNRESOLVED"))?;
        if result.try_get::<i64>("", "n")? > 0 {
            bail!("RESET_SCHEMA_CONTAINS_FOREIGN_TENANT_ROWS");
        }
    }
    let (delete_order, clear_nullable_links) = crate::reset_order::plan(deletes, dependencies)?;
    Ok(ResetManifest {
        delete_order,
        truncate_tables: options.reset_truncate_tables.clone(),
        backup: options.replacement_backup.clone(),
        retained_tables: plan
            .tables
            .iter()
            .filter(|table| !reset_set.contains(table.name.as_str()))
            .map(|t| t.name.clone())
            .collect(),
        preserved_user_ids: users,
        preserved_employee_ids: employees,
        clear_nullable_links,
    })
}
pub async fn apply<C: ConnectionTrait>(
    db: &C,
    schema: &str,
    manifest: &ResetManifest,
) -> Result<()> {
    if !crate::options::valid_schema(schema) {
        bail!("RESET_SCHEMA_INVALID");
    }
    if !manifest.truncate_tables.is_empty() {
        for table in &manifest.truncate_tables {
            if !safe_identifier(table)
                || foundational(table, schema)
                || matches!(table.as_str(), "user" | "employee" | "user_role")
                || manifest.delete_order.contains(table)
                || manifest.retained_tables.contains(table)
            {
                bail!("RESET_TRUNCATE_PRESERVED_TABLE");
            }
        }
        let targets = manifest
            .truncate_tables
            .iter()
            .map(|table| format!("ONLY \"{schema}\".\"{table}\""))
            .collect::<Vec<_>>()
            .join(",");
        db.execute(Statement::from_string(
            DbBackend::Postgres,
            format!("TRUNCATE TABLE {targets} CONTINUE IDENTITY RESTRICT"),
        ))
        .await?;
    }
    for link in &manifest.clear_nullable_links {
        if !manifest.delete_order.contains(&link.child)
            || !manifest.delete_order.contains(&link.parent)
            || foundational(&link.child, schema)
            || !safe_identifier(&link.child)
            || !safe_identifier(&link.parent)
            || link
                .clear_columns
                .iter()
                .chain(link.child_columns.iter())
                .chain(link.parent_columns.iter())
                .any(|column| !safe_identifier(column))
        {
            bail!("RESET_NULLABLE_LINK_INVALID");
        }
        let assignments = link
            .clear_columns
            .iter()
            .map(|column| format!("\"{column}\"=NULL"))
            .collect::<Vec<_>>()
            .join(",");
        let join = link
            .child_columns
            .iter()
            .zip(&link.parent_columns)
            .map(|(child, parent)| format!("c.\"{child}\"=p.\"{parent}\""))
            .collect::<Vec<_>>()
            .join(" AND ");
        let sql=format!("UPDATE \"{schema}\".\"{}\" c SET {assignments} FROM \"{schema}\".\"{}\" p WHERE {join} AND {} AND {} AND cardinality($1::uuid[])>=0 AND cardinality($2::uuid[])>=0",link.child,link.parent,qualified_selector(&link.child,"c"),qualified_selector(&link.parent,"p"));
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            arrays(
                &manifest.preserved_user_ids,
                &manifest.preserved_employee_ids,
            ),
        ))
        .await?;
    }
    for table in &manifest.delete_order {
        if !safe_identifier(table) || foundational(table, schema) {
            bail!("RESET_TABLE_MANIFEST_INVALID");
        }
        // Reference both typed arrays so parameter numbering is identical for every selector.
        let sql=format!("DELETE FROM \"{schema}\".\"{table}\" WHERE {} AND cardinality($1::uuid[])>=0 AND cardinality($2::uuid[])>=0",selector(table));
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            sql,
            arrays(
                &manifest.preserved_user_ids,
                &manifest.preserved_employee_ids,
            ),
        ))
        .await?;
    }
    Ok(())
}
