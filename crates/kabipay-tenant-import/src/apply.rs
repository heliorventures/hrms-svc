//! One tenant transaction, independent section savepoints, committed audit and replay.
use crate::{
    contract::ImportPackage,
    preview::{self, ImportPlan},
    report::ImportReport,
    tenant_target::VerifiedTenantTarget,
};
use anyhow::{bail, Result};
use kabipay_db_entities::tenant::d0092_tenant_import_tracking::tenant_import_run;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};
use std::path::Path;
use uuid::Uuid;

pub struct Execution<'a> {
    pub confirm_digest: &'a str,
    pub replace: bool,
    pub write_paused: bool,
    pub output: &'a Path,
    pub pg_bin: Option<&'a Path>,
}

pub async fn apply(
    target: &VerifiedTenantTarget,
    package: &ImportPackage,
    plan: &ImportPlan,
    execution: Execution<'_>,
) -> Result<ImportReport> {
    if package
        .issues
        .iter()
        .any(|issue| issue.severity == "BLOCK_TENANT")
    {
        bail!("SOURCE_BLOCKS_TENANT_IMPORT");
    }
    let options = &target.options;
    let committed = tenant_import_run::Entity::find()
        .filter(tenant_import_run::Column::TenantId.eq(options.tenant_id))
        .filter(tenant_import_run::Column::PackageHash.eq(&plan.package_hash))
        .filter(tenant_import_run::Column::ConfigurationHash.eq(&plan.configuration_hash))
        .filter(tenant_import_run::Column::Mode.eq(&plan.mode))
        .one(&target.db)
        .await?;
    if let Some(run) = committed {
        let mut report: ImportReport = serde_json::from_value(run.report)?;
        for item in &mut report.sections {
            if matches!(item.outcome.as_str(), "CREATED" | "UPDATED") {
                item.outcome = "UNCHANGED".into();
                item.code = "RUN_ALREADY_COMMITTED".into();
            }
        }
        crate::private_output::write_json(execution.output, "report.json", &report)?;
        return Ok(report);
    }
    if execution.confirm_digest != plan.digest || (execution.replace && !execution.write_paused) {
        bail!("REVIEWED_DIGEST_OR_WRITE_PAUSE_REQUIRED");
    }
    if execution.replace && (plan.core_ready_rows != plan.source_rows || plan.blocking_issues > 0) {
        bail!("REPLACEMENT_REQUIRES_ALL_EMPLOYEE_IDENTITIES_RESOLVED");
    }
    let mapping_lock = crate::tenant_target::lock_mapping(target).await?;
    let transaction = target.db.begin().await?;
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "SET TRANSACTION ISOLATION LEVEL REPEATABLE READ",
        ))
        .await?;
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "SET LOCAL lock_timeout='15s'",
        ))
        .await?;
    preview::lock_tables(&transaction, &options.schema_name, &plan.tables).await?;
    transaction
        .execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
            [format!("tenant-import:{}", options.tenant_id).into()],
        ))
        .await?;
    let current = preview::preview(
        &transaction,
        package,
        options,
        &plan.package_hash,
        execution.replace,
    )
    .await?;
    if current.digest != execution.confirm_digest {
        bail!("TARGET_CHANGED_REVIEW_A_NEW_PREVIEW");
    }
    let actor = transaction
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id FROM \"user\" WHERE tenant_id=$1 AND id=$2 AND is_active AND NOT is_deleted",
            [options.tenant_id.into(), options.actor_id.into()],
        ))
        .await?;
    if actor.is_none() {
        bail!("IMPORT_ACTOR_INACTIVE");
    }
    let permissions = kabipay_auth::rbac::load_client_authorization(
        &transaction,
        options.tenant_id,
        options.actor_id,
    )
    .await?;
    for code in [
        kabipay_common::context::PERM_EMPLOYEE_MANAGE,
        kabipay_common::context::PERM_PAYROLL_MANAGE,
        kabipay_common::context::PERM_LEAVE_MANAGE,
    ] {
        if permissions
            .permission_scopes
            .get(code)
            .is_none_or(|scope| scope != "ALL")
        {
            bail!("IMPORT_ACTOR_UNAUTHORIZED");
        }
    }
    if execution.replace {
        let manifest = current
            .reset
            .as_ref()
            .ok_or_else(|| anyhow::anyhow!("RESET_REVIEW_REQUIRED"))?;
        let bin = execution
            .pg_bin
            .ok_or_else(|| anyhow::anyhow!("BACKUP_TOOLS_REQUIRED"))?;
        crate::backup::backup(
            &transaction,
            options,
            &target.connection_host,
            execution.output,
            bin,
        )
        .await?;
        crate::reset::apply(&transaction, &options.schema_name, manifest).await?;
    }
    for id in &options.payroll_excluded_employee_ids {
        let row = transaction
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT payroll_excluded FROM employee WHERE tenant_id=$1 AND id=$2",
                [options.tenant_id.into(), (*id).into()],
            ))
            .await?
            .ok_or_else(|| anyhow::anyhow!("PAYROLL_EXCLUSION_ID_UNRESOLVED"))?;
        if !row.try_get::<bool>("", "payroll_excluded")? {
            transaction
                .execute(Statement::from_sql_and_values(
                    DbBackend::Postgres,
                    "UPDATE employee SET payroll_excluded=TRUE WHERE tenant_id=$1 AND id=$2",
                    [options.tenant_id.into(), (*id).into()],
                ))
                .await?;
        }
    }
    let mut report = ImportReport {
        run_id: Uuid::new_v4(),
        tenant_id: options.tenant_id,
        committed: false,
        sections: Vec::new(),
        issues: package
            .issues
            .iter()
            .map(|issue| crate::report::SourceIssue {
                source_ref: issue.source_ref.clone(),
                severity: issue.severity.clone(),
                section: issue.section.clone(),
                code: issue.code.clone(),
                field: issue.field.clone(),
            })
            .collect(),
    };
    let mut credentials = Vec::new();
    for row in &package.employees {
        let blocked = package.issues.iter().any(|issue| {
            issue.severity == "BLOCK_EMPLOYEE"
                && issue.source_ref.as_ref().is_some_and(|source| {
                    source.sheet == row.source_ref.sheet && source.row == row.source_ref.row
                })
        });
        if blocked || row.core_ready().is_err() {
            for section in [
                "employee",
                "profile",
                "department",
                "designation",
                "identity",
                "bank",
                "recurring_salary",
                "leave_opening",
                "period_input",
            ] {
                report.record(
                    &row.source_ref,
                    section,
                    "DEFERRED",
                    "CORE_IDENTITY_UNRESOLVED",
                );
            }
            continue;
        }
        let core = transaction.begin().await?;
        let credential_count = credentials.len();
        let result = crate::employee_import::core(&core, row, options).await;
        let employee = match result {
            Ok((id, outcome)) => {
                core.commit().await?;
                report.record(&row.source_ref, "employee", outcome, "CORE_IMPORTED");
                id
            }
            Err(_) => {
                core.rollback().await?;
                credentials.truncate(credential_count);
                report.record(&row.source_ref, "employee", "FAILED", "CORE_IMPORT_FAILED");
                for section in [
                    "profile",
                    "department",
                    "designation",
                    "identity",
                    "bank",
                    "recurring_salary",
                    "leave_opening",
                    "period_input",
                ] {
                    report.record(&row.source_ref, section, "DEFERRED", "CORE_IMPORT_FAILED");
                }
                continue;
            }
        };
        let login = transaction.begin().await?;
        let before = credentials.len();
        match crate::login_import::save(&login, row, options, employee, &mut credentials).await {
            Ok(outcome) => {
                login.commit().await?;
                report.record(&row.source_ref, "login", outcome, "LOGIN_IMPORTED");
            }
            Err(error) => {
                login.rollback().await?;
                credentials.truncate(before);
                report.record(
                    &row.source_ref,
                    "login",
                    "DEFERRED",
                    &crate::cli::safe_error(&error),
                );
            }
        }
        for section in [
            "profile",
            "department",
            "designation",
            "identity",
            "bank",
            "recurring_salary",
            "leave_opening",
            "period_input",
        ] {
            let savepoint = transaction.begin().await?;
            let result = crate::import_sections::write_section(
                &savepoint, package, row, options, employee, section,
            )
            .await;
            match result {
                Ok((outcome, code)) => {
                    savepoint.commit().await?;
                    report.record(&row.source_ref, section, outcome, code);
                }
                Err(error) => {
                    savepoint.rollback().await?;
                    report.record(
                        &row.source_ref,
                        section,
                        "DEFERRED",
                        &crate::cli::safe_error(&error),
                    );
                }
            }
        }
        if row.employee.exit_date.is_some() || row.employee.last_working_date.is_some() {
            report.record(
                &row.source_ref,
                "lifecycle_review",
                "DEFERRED",
                "SOURCE_DATES_STORED_APPROVAL_NOT_INFERRED",
            );
        }
        if row
            .recurring_salary
            .as_ref()
            .is_some_and(|s| s["annual_employer_pf"].is_null())
        {
            report.record(
                &row.source_ref,
                "employer_cost",
                "DEFERRED",
                "EMPLOYER_COST_RULE_UNRESOLVED",
            );
        }
    }
    // Write staged credentials before commit; only the persisted run proves they may be distributed.
    crate::private_output::write_json(
        execution.output,
        "run-state.staged.json",
        &serde_json::json!({
        "run_id":report.run_id,"tenant_id":report.tenant_id,"package_hash":plan.package_hash,
        "configuration_hash":plan.configuration_hash,"committed":false}),
    )?;
    if !credentials.is_empty() {
        crate::private_output::write_json(
            execution.output,
            "credentials.staged.json",
            &credentials,
        )?;
    }
    crate::import_audit::persist(&transaction, &report, package, plan, options.actor_id).await?;
    transaction.commit().await?;
    mapping_lock.rollback().await?;
    report.committed = true;
    crate::private_output::write_json(execution.output, "report.json", &report)?;
    Ok(report)
}
