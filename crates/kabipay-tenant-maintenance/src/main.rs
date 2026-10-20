//! Source-owned business maintenance, independently deployed from transport delivery.
mod migrate_legacy;
use anyhow::Context;
use chrono::Utc;
use kabipay_common::db::{
    connect_ops_db, resolve_required_tenant_db, TenantDbCache, TenantDbConfig,
};
use kabipay_common::due_offboarding::process_due_separations;
use kabipay_common::entitlements::Entitlements;
use kabipay_common::load_dotenv;
use kabipay_common::private_file_cleanup::{
    process_private_file_cleanup_tasks, sweep_expired_company_upload_stages,
};
use kabipay_common::subgraph::{ops_dsn_from_env, tenant_db_config_from_env};
use kabipay_common::telemetry::init_tracing;
use kabipay_common::tenant_business_clock::TenantBusinessClock;
use kabipay_db_entities::ops::tenant_database;
use kabipay_notification::services::automated_events::process_due_celebrations;
use kabipay_performance::services::performance_workflow::{
    process_due_performance_cycles, process_due_performance_stage_deadlines,
};
use kabipay_survey::services::survey_lifecycle::process_due_surveys;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use std::{collections::HashSet, time::Duration};
use uuid::Uuid;
fn poll_interval() -> Duration {
    let milliseconds = std::env::var("KABIPAY_TENANT_MAINTENANCE_POLL_MS")
        .ok()
        .and_then(|v| v.parse::<u64>().ok())
        .unwrap_or(2000);
    Duration::from_millis(milliseconds.clamp(250, 60_000))
}
async fn active_tenant_ids(
    ops: &DatabaseConnection,
    bound_tenant: Option<Uuid>,
) -> anyhow::Result<Vec<Uuid>> {
    let mut query =
        tenant_database::Entity::find().filter(tenant_database::Column::IsActive.eq(true));
    if let Some(tenant_id) = bound_tenant {
        query = query.filter(tenant_database::Column::TenantId.eq(tenant_id));
    }
    let rows = query.all(ops).await.context("list tenant_database")?;
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for r in rows {
        if seen.insert(r.tenant_id) {
            out.push(r.tenant_id);
        }
    }
    Ok(out)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();
    init_tracing("kabipay-tenant-maintenance");

    let cache = TenantDbCache::from_env()?;
    let bound_tenant = cache
        .bound_tenant_id()
        .context("Tenant maintenance requires a bound company source")?;
    let dsn = ops_dsn_from_env();
    let ops_db = connect_ops_db(&dsn)
        .await
        .map_err(|e| anyhow::anyhow!("ops database connection failed: {e}"))?;
    cache.validate_binding_registration(&ops_db).await?;
    let fallback: TenantDbConfig = tenant_db_config_from_env();
    let args: Vec<String> = std::env::args().skip(1).collect();
    if !args.is_empty() {
        let db = resolve_required_tenant_db(bound_tenant, &ops_db, &cache, &fallback).await?;
        match args.as_slice() {
            [mode, path] if mode == "--migrate-delivery" => {
                migrate_legacy::run(&db, bound_tenant, path, false).await?
            }
            [mode, path, execute] if mode == "--migrate-delivery" && execute == "--execute" => {
                migrate_legacy::run(&db, bound_tenant, path, true).await?
            }
            [mode] if mode == "--verify-delivery-ownership" => {
                migrate_legacy::verify_ownership(&db, bound_tenant).await?
            }
            _ => anyhow::bail!("Unsupported maintenance operator arguments"),
        }
        return Ok(());
    }
    let interval = poll_interval();

    tracing::info!(poll_ms = interval.as_millis(), "tenant maintenance started");

    loop {
        match active_tenant_ids(&ops_db, Some(bound_tenant)).await {
            Ok(tenants) => {
                for tid in tenants {
                    match resolve_required_tenant_db(tid, &ops_db, &cache, &fallback).await {
                        Ok(tdb) => {
                            match TenantBusinessClock::load(&ops_db, tid).await {
                                Ok(clock) => {
                                    let now = Utc::now();
                                    let business_date = clock.business_date(now);
                                    let business_time = clock.local_time(now);
                                    match process_due_separations(&tdb, tid, business_date).await {
                                        Ok(result) if result.processed > 0 => {
                                            tracing::info!(%tid, processed = result.processed, "due employee offboarding completed");
                                        }
                                        Ok(_) => {}
                                        Err(error) => {
                                            tracing::error!(%tid, code = error.code(), "due employee offboarding sweep failed")
                                        }
                                    }
                                    let (employee_enabled, attendance_enabled) =
                                        match Entitlements::load(&ops_db, tid).await {
                                            Ok(state) => (
                                                state.allows("EMPLOYEE"),
                                                state.allows("ATTENDANCE"),
                                            ),
                                            Err(error) => {
                                                tracing::error!(%tid, code = error.code(), "scheduled domain work held: entitlements unavailable");
                                                (false, false)
                                            }
                                        };
                                    if attendance_enabled {
                                        match kabipay_attendance::sweep_expired_attendance(
                                            &tdb, tid, clock, 50,
                                        )
                                        .await
                                        {
                                            Ok(result)
                                                if result.expired > 0 || result.failed > 0 =>
                                            {
                                                tracing::info!(
                                                    %tid, expired = result.expired, failed = result.failed, "attendance expiry sweep completed"
                                                )
                                            }
                                            Ok(_) => {}
                                            Err(error) => {
                                                tracing::error!(%tid, code = error.code(), "attendance expiry sweep failed")
                                            }
                                        }
                                    }
                                    if employee_enabled {
                                        match process_due_surveys(&tdb, tid).await {
                                            Ok(result)
                                                if result.opened > 0
                                                    || result.closed > 0
                                                    || result.failed > 0 =>
                                            {
                                                tracing::info!(
                                                    %tid, opened = result.opened, closed = result.closed, failed = result.failed,
                                                    "scheduled survey sweep completed"
                                                )
                                            }
                                            Ok(_) => {}
                                            Err(error) => {
                                                tracing::error!(%tid, code = error.code(), "scheduled survey sweep failed")
                                            }
                                        }
                                        match process_due_celebrations(
                                            &tdb,
                                            tid,
                                            business_date,
                                            business_time,
                                        )
                                        .await
                                        {
                                            Ok(result) if result.notifications_created > 0 => {
                                                tracing::info!(
                                                    %tid,
                                                    eligible_events = result.eligible_events,
                                                    notifications_created = result.notifications_created,
                                                    duplicates_skipped = result.duplicates_skipped,
                                                    "automated employee notification sweep completed"
                                                );
                                            }
                                            Ok(_) => {}
                                            Err(error) => tracing::error!(
                                                %tid,
                                                code = error.code(),
                                                "automated employee notification sweep failed"
                                            ),
                                        }
                                        match process_due_performance_cycles(
                                            &tdb,
                                            tid,
                                            business_date,
                                        )
                                        .await
                                        {
                                            Ok(result) if result.cycles_created > 0 => {
                                                tracing::info!(
                                                    %tid,
                                                    cycles_created = result.cycles_created,
                                                    participants_created = result.participants_created,
                                                    "scheduled performance cycle sweep completed"
                                                )
                                            }
                                            Ok(_) => {}
                                            Err(error) => tracing::error!(
                                                %tid,
                                                code = error.code(),
                                                "scheduled performance cycle sweep failed"
                                            ),
                                        }
                                        match process_due_performance_stage_deadlines(
                                            &tdb,
                                            tid,
                                            business_date,
                                        )
                                        .await
                                        {
                                            Ok(result)
                                                if result.stages_advanced > 0
                                                    || result.stages_blocked > 0 =>
                                            {
                                                tracing::info!(%tid, stages_advanced = result.stages_advanced, stages_blocked = result.stages_blocked, "scheduled performance stage sweep completed")
                                            }
                                            Ok(_) => {}
                                            Err(error) => {
                                                tracing::error!(%tid, code = error.code(), "scheduled performance stage sweep failed")
                                            }
                                        }
                                    }
                                }
                                Err(error) => tracing::error!(
                                    %tid,
                                    code = error.code(),
                                    "tenant business clock unavailable for scheduled tenant sweeps"
                                ),
                            }
                            if let Err(error) =
                                sweep_expired_company_upload_stages(&tdb, tid, 25).await
                            {
                                tracing::error!(%tid, code = error.code(), "expired file-upload stage sweep failed");
                            }
                            if let Err(error) =
                                kabipay_notification::services::announcement_video::sweep_expired(
                                    &tdb, tid, 25,
                                )
                                .await
                            {
                                tracing::error!(%tid, code=error.code(), "expired video upload sweep failed");
                            }
                            if let Err(error) =
                                process_private_file_cleanup_tasks(&tdb, tid, 25).await
                            {
                                tracing::error!(%tid, code = error.code(), "private file cleanup sweep failed");
                            }
                        }
                        Err(e) => tracing::error!(%tid, error = %e, "resolve tenant db failed"),
                    }
                }
            }
            Err(e) => tracing::error!(error = %e, "list tenants failed"),
        }
        tokio::select! {
            _ = tokio::time::sleep(interval) => {},
            result = tokio::signal::ctrl_c() => { result?; break; }
        }
    }
    Ok(())
}
