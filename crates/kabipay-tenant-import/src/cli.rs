//! Explicit operator commands. Validation is offline; preview performs reads only.
use crate::{contract::ImportPackage, options::ImportOptions, preview::ImportPlan};
use anyhow::{bail, Result};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter};
use std::{
    collections::BTreeMap,
    path::{Path, PathBuf},
};

pub struct Arguments {
    pub command: String,
    pub values: BTreeMap<String, String>,
    pub replace: bool,
    pub write_paused: bool,
}
impl Arguments {
    pub fn parse(args: impl IntoIterator<Item = String>) -> Result<Self> {
        let mut args = args.into_iter();
        let command = args.next().unwrap_or_else(|| "help".into());
        if !matches!(
            command.as_str(),
            "help" | "validate" | "preview" | "apply" | "reconcile"
        ) {
            bail!("COMMAND_INVALID");
        }
        let mut values = BTreeMap::new();
        let mut replace = false;
        let mut write_paused = false;
        while let Some(flag) = args.next() {
            match flag.as_str() {
                "--replace" if !replace => replace = true,
                "--writes-paused" if !write_paused => write_paused = true,
                "--package" | "--options" | "--output" | "--plan" | "--confirm" | "--pg-bin"
                | "--env-file" | "--run-id" => {
                    let value = args
                        .next()
                        .filter(|v| !v.starts_with("--"))
                        .ok_or_else(|| anyhow::anyhow!("ARGUMENT_VALUE_REQUIRED"))?;
                    if values.insert(flag, value).is_some() {
                        bail!("DUPLICATE_ARGUMENT");
                    }
                }
                _ => bail!("ARGUMENT_INVALID"),
            }
        }
        if command != "apply"
            && (write_paused || values.contains_key("--confirm") || values.contains_key("--plan"))
        {
            bail!("APPLY_FLAGS_REQUIRE_APPLY_COMMAND");
        }
        Ok(Self {
            command,
            values,
            replace,
            write_paused,
        })
    }
    fn required(&self, key: &str) -> Result<&str> {
        self.values
            .get(key)
            .map(String::as_str)
            .ok_or_else(|| anyhow::anyhow!("REQUIRED_ARGUMENT_MISSING"))
    }
}
fn bounded_read(path: &str) -> Result<Vec<u8>> {
    if std::fs::metadata(path)?.len() > 20 * 1024 * 1024 {
        bail!("INPUT_FILE_TOO_LARGE");
    }
    Ok(std::fs::read(path)?)
}
pub async fn run(args: Arguments) -> Result<()> {
    if args.command == "help" {
        println!("tenant-import validate|preview|apply|reconcile --package PATH --options PATH --output NEW_DIRECTORY\napply additionally requires --plan PATH --confirm DIGEST; replacement also requires --replace --writes-paused. Backup defaults to REQUIRED with --pg-bin PATH; an explicit reviewed replacement_backup SKIP reason may be supplied in options. Optional --env-file PATH. No command migrates or deploys.");
        return Ok(());
    }
    let bytes = bounded_read(args.required("--package")?)?;
    let package = ImportPackage::parse(&bytes)?;
    if args.command == "validate" {
        let rows = crate::validation::validate(&package);
        if let Some(path) = args.values.get("--output") {
            let output = crate::private_output::create(&PathBuf::from(path))?;
            crate::private_output::write_json(
                &output,
                "validation-report.json",
                &serde_json::json!({
                "database_writes":false,"rows":rows,"financial_checks":"SHARED_DOMAIN_VALIDATORS",
                "tenant_target_checked":false}),
            )?;
        }
        println!("PACKAGE_VALID rows={} core_ready={} salary_ready={} leave_ready={} period_ready={} database_writes=false",
            rows.len(),rows.iter().filter(|r|r.core_ready).count(),rows.iter().filter(|r|r.salary_ready).count(),
            rows.iter().filter(|r|r.leave_ready).count(),rows.iter().filter(|r|r.period_ready).count());
        return Ok(());
    }
    let options: ImportOptions =
        serde_json::from_slice(&bounded_read(args.required("--options")?)?)?;
    options.validate()?;
    if options.login_by_employee_code.keys().any(|code| {
        !package
            .employees
            .iter()
            .any(|row| row.employee.code.as_ref() == Some(code))
    }) {
        bail!("LOGIN_MANIFEST_EMPLOYEE_UNRESOLVED");
    }
    if let Some(path) = args.values.get("--env-file") {
        dotenvy::from_path(path)?;
    }
    for key in [
        "DATABASE_URL",
        "POSTGRES_HOST",
        "POSTGRES_PORT",
        "POSTGRES_DB",
        "POSTGRES_USER",
        "POSTGRES_PASSWORD",
    ] {
        if std::env::var(key).is_err() {
            bail!("EXPLICIT_CONNECTION_ENVIRONMENT_REQUIRED");
        }
    }
    let output = crate::private_output::create(&PathBuf::from(args.required("--output")?))?;
    let result = run_connected(args, options, package, bytes, &output).await;
    if let Err(error) = &result {
        if crate::private_output::write_json(
            &output,
            "failure.json",
            &crate::failure::describe(error),
        )
        .is_err()
        {
            eprintln!("FAILURE_REPORT_WRITE_FAILED");
        }
    }
    result
}

async fn run_connected(
    args: Arguments,
    options: ImportOptions,
    package: ImportPackage,
    bytes: Vec<u8>,
    output: &Path,
) -> Result<()> {
    let ops =
        kabipay_common::db::connect_ops_db(&kabipay_common::subgraph::ops_dsn_from_env()).await?;
    let target = crate::tenant_target::resolve(
        &ops,
        &kabipay_common::subgraph::tenant_db_config_from_env(),
        &package,
        &options,
    )
    .await?;
    let package_hash = crate::preview::hash(&bytes);
    match args.command.as_str() {
        "preview" => {
            let plan = crate::preview::preview(
                &target.db,
                &package,
                &options,
                &package_hash,
                args.replace,
            )
            .await?;
            crate::private_output::write_json(output, "plan.json", &plan)?;
            println!(
                "PREVIEW_ONLY rows={} core_ready={} period_ready={} blocked={} digest={}",
                plan.source_rows,
                plan.core_ready_rows,
                plan.period_ready_rows,
                plan.blocking_issues,
                plan.digest
            );
        }
        "apply" => {
            let plan: ImportPlan =
                serde_json::from_slice(&bounded_read(args.required("--plan")?)?)?;
            let mode = if args.replace { "REPLACE" } else { "IMPORT" };
            if plan.tenant_id != options.tenant_id
                || plan.schema_name != options.schema_name
                || plan.mode != mode
                || plan.package_hash != package_hash
                || plan.configuration_hash != crate::preview::hash(&serde_json::to_vec(&options)?)
            {
                bail!("PLAN_INPUTS_DISAGREE");
            }
            let pg_bin = args.values.get("--pg-bin").map(PathBuf::from);
            let report = crate::apply::apply(
                &target,
                &package,
                &plan,
                crate::apply::Execution {
                    confirm_digest: args.required("--confirm")?,
                    replace: args.replace,
                    write_paused: args.write_paused,
                    output,
                    pg_bin: pg_bin.as_deref(),
                },
            )
            .await?;
            println!("IMPORT_COMMITTED run_id={} created={} updated={} unchanged={} deferred={} failed={}",report.run_id,
                report.count("CREATED"),report.count("UPDATED"),report.count("UNCHANGED"),report.count("DEFERRED"),report.count("FAILED"));
        }
        "reconcile" => {
            use kabipay_db_entities::tenant::d0092_tenant_import_tracking::tenant_import_run;
            let id = uuid::Uuid::parse_str(args.required("--run-id")?)?;
            let run = tenant_import_run::Entity::find()
                .filter(tenant_import_run::Column::TenantId.eq(options.tenant_id))
                .filter(tenant_import_run::Column::Id.eq(id))
                .one(&target.db)
                .await?
                .ok_or_else(|| anyhow::anyhow!("RUN_NOT_COMMITTED"))?;
            crate::private_output::write_json(output, "committed-report.json", &run.report)?;
            println!("RUN_CONFIRMED_COMMITTED run_id={id}");
        }
        _ => bail!("COMMAND_INVALID"),
    }
    target.db.close().await?;
    ops.close().await?;
    Ok(())
}
pub fn safe_error(error: &anyhow::Error) -> String {
    let message = error.to_string();
    let code = match message.as_str() {
        "validation error: additional deduction reason is required" => {
            Some("DEDUCTION_REASON_REQUIRED")
        }
        "validation error: earned components do not reconcile to gross" => {
            Some("EARNED_COMPONENT_RECONCILIATION_REQUIRED")
        }
        "validation error: leave balance was adjusted; reviewed reconciliation is required" => {
            Some("LEAVE_BALANCE_RECONCILIATION_REQUIRED")
        }
        _ => None,
    };
    if let Some(code) = code {
        return code.into();
    }
    if message.len() <= 100 && message.bytes().all(|b| b.is_ascii_uppercase() || b == b'_') {
        message
    } else {
        "OPERATION_FAILED_REVIEW_CONFIGURATION_AND_PRIVATE_REPORT".into()
    }
}
