//! Dry-run-first migration of legacy tenant-local attendance wall times to UTC instants.

use anyhow::{bail, Context};
use chrono::{DateTime, Days, NaiveDate, NaiveTime, Utc};
use kabipay_common::db::{
    connect_ops_db, resolve_required_tenant_db, TenantDbCache, TenantDbConfig,
};
use kabipay_common::subgraph::{ops_dsn_from_env, tenant_db_config_from_env};
use kabipay_common::tenant_business_clock::TenantBusinessClock;
use kabipay_common::{load_dotenv, KabiPayError};
use kabipay_db_entities::ops::tenant_database;
use kabipay_db_entities::tenant::d0010_time_shift_roster::attendance;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement, TransactionTrait,
};
use uuid::Uuid;

#[derive(Debug, Clone, Copy)]
struct Options {
    apply: bool,
    tenant_id: Option<Uuid>,
}

#[derive(Debug, Default)]
struct TenantResult {
    eligible: u64,
    converted: u64,
    audited: u64,
    skipped: u64,
}

fn parse_options() -> anyhow::Result<Options> {
    let mut apply = false;
    let mut tenant_id = None;
    let mut args = std::env::args().skip(1);
    while let Some(arg) = args.next() {
        match arg.as_str() {
            "--apply" => apply = true,
            "--tenant-id" => {
                let raw = args.next().context("--tenant-id requires a UUID")?;
                tenant_id = Some(raw.parse().context("--tenant-id must be a UUID")?);
            }
            "--help" | "-h" => {
                println!(
                    "Usage: backfill_attendance_instants [--tenant-id UUID] [--apply]\n\
                     Dry-run is the default. Writes require both --apply and --tenant-id UUID."
                );
                std::process::exit(0);
            }
            other => bail!("unknown argument: {other}"),
        }
    }
    if apply && tenant_id.is_none() {
        bail!("--apply requires --tenant-id UUID; bulk writes are intentionally disabled");
    }
    Ok(Options { apply, tenant_id })
}

fn legacy_instants(
    clock: TenantBusinessClock,
    row: &attendance::Model,
) -> Result<(DateTime<Utc>, Option<DateTime<Utc>>), KabiPayError> {
    let check_in_time = row.check_in_time.ok_or_else(|| {
        KabiPayError::Validation("legacy attendance row has no check-in time".into())
    })?;
    let check_in_at = clock.to_utc(row.work_date, check_in_time)?;
    let Some(check_out_time) = row.check_out_time else {
        return Ok((check_in_at, None));
    };
    let check_out_date = checkout_date(row.work_date, check_in_time, check_out_time)?;
    let check_out_at = clock.to_utc(check_out_date, check_out_time)?;
    let duration = check_out_at - check_in_at;
    if duration.num_seconds() <= 0 || duration.num_seconds() >= 24 * 60 * 60 {
        return Err(KabiPayError::Validation(
            "legacy attendance duration must be greater than zero and less than 24 hours".into(),
        ));
    }
    Ok((check_in_at, Some(check_out_at)))
}

fn checkout_date(
    work_date: NaiveDate,
    check_in: NaiveTime,
    check_out: NaiveTime,
) -> Result<NaiveDate, KabiPayError> {
    if check_out == check_in {
        return Err(KabiPayError::Validation(
            "equal legacy punch times represent a disallowed 24-hour segment".into(),
        ));
    }
    if check_out > check_in {
        Ok(work_date)
    } else {
        work_date.checked_add_days(Days::new(1)).ok_or_else(|| {
            KabiPayError::Validation("legacy checkout date is outside supported range".into())
        })
    }
}

async fn audit_failure(
    txn: &sea_orm::DatabaseTransaction,
    row: &attendance::Model,
    timezone: &str,
    reason: &str,
) -> anyhow::Result<()> {
    let Some(legacy_time) = row.check_in_time else {
        return Ok(());
    };
    txn.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"INSERT INTO attendance_instant_backfill_audit
           (id, tenant_id, attendance_id, field_name, timezone, legacy_work_date,
            legacy_time, reason, created_at)
           VALUES ($1, $2, $3, 'segment', $4, $5, $6, $7, NOW())
           ON CONFLICT (attendance_id, field_name) DO NOTHING"#,
        vec![
            Uuid::new_v4().into(),
            row.tenant_id.into(),
            row.id.into(),
            timezone.into(),
            row.work_date.into(),
            legacy_time.into(),
            reason.chars().take(100).collect::<String>().into(),
        ],
    ))
    .await?;
    Ok(())
}

async fn process_tenant(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    clock: TenantBusinessClock,
    apply: bool,
) -> anyhow::Result<TenantResult> {
    let rows = attendance::Entity::find()
        .filter(attendance::Column::TenantId.eq(tenant_id))
        .filter(
            sea_orm::Condition::any()
                .add(attendance::Column::CheckInAt.is_null())
                .add(
                    sea_orm::Condition::all()
                        .add(attendance::Column::CheckOutTime.is_not_null())
                        .add(attendance::Column::CheckOutAt.is_null()),
                ),
        )
        .all(db)
        .await?;

    let mut result = TenantResult {
        eligible: rows.len() as u64,
        ..TenantResult::default()
    };
    for row in rows {
        match legacy_instants(clock, &row) {
            Ok((check_in_at, check_out_at)) if apply => {
                let txn = db.begin().await?;
                let update = txn
                    .execute(Statement::from_sql_and_values(
                        DbBackend::Postgres,
                        r#"UPDATE attendance
                           SET check_in_at = COALESCE(check_in_at, $3),
                               check_out_at = CASE
                                   WHEN check_out_time IS NOT NULL THEN COALESCE(check_out_at, $4)
                                   ELSE check_out_at
                               END,
                               updated_at = NOW()
                           WHERE id = $1 AND tenant_id = $2
                             AND (check_in_at IS NULL OR (check_out_time IS NOT NULL AND check_out_at IS NULL))"#,
                        vec![
                            row.id.into(),
                            tenant_id.into(),
                            check_in_at.into(),
                            check_out_at.into(),
                        ],
                    ))
                    .await?;
                txn.commit().await?;
                result.converted += update.rows_affected();
                result.skipped += u64::from(update.rows_affected() == 0);
            }
            Ok(_) => result.converted += 1,
            Err(error) if apply => {
                let txn = db.begin().await?;
                audit_failure(&txn, &row, clock.timezone_name(), &error.to_string()).await?;
                txn.commit().await?;
                result.audited += 1;
            }
            Err(_) => result.audited += 1,
        }
    }
    Ok(result)
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    load_dotenv();
    let options = parse_options()?;
    let ops_db = connect_ops_db(&ops_dsn_from_env()).await?;
    let fallback: TenantDbConfig = tenant_db_config_from_env();
    let cache = TenantDbCache::new();
    let mut query = tenant_database::Entity::find()
        .filter(tenant_database::Column::IsActive.eq(true));
    if let Some(tenant_id) = options.tenant_id {
        query = query.filter(tenant_database::Column::TenantId.eq(tenant_id));
    }
    let tenants = query.all(&ops_db).await?;
    if tenants.is_empty() {
        bail!("no active tenant database matched the requested scope");
    }

    println!(
        "attendance instant backfill mode={} tenants={}",
        if options.apply { "APPLY" } else { "DRY_RUN" },
        tenants.len()
    );
    for tenant in tenants {
        let tenant_id = tenant.tenant_id;
        let clock = TenantBusinessClock::load(&ops_db, tenant_id).await?;
        let db = resolve_required_tenant_db(tenant_id, &ops_db, &cache, &fallback).await?;
        let result = process_tenant(&db, tenant_id, clock, options.apply).await?;
        println!(
            "tenant={} timezone={} eligible={} converted={} audited={} skipped={}",
            tenant_id,
            clock.timezone_name(),
            result.eligible,
            result.converted,
            result.audited,
            result.skipped
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn date(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }

    fn time(value: &str) -> NaiveTime {
        value.parse().unwrap()
    }

    #[test]
    fn overnight_checkout_uses_the_following_business_date() {
        assert_eq!(
            checkout_date(date("2026-08-24"), time("22:00:00"), time("06:00:00")).unwrap(),
            date("2026-08-25")
        );
    }

    #[test]
    fn equal_wall_times_are_not_silently_interpreted_as_twenty_four_hours() {
        assert!(checkout_date(
            date("2026-08-24"),
            time("09:00:00"),
            time("09:00:00")
        )
        .is_err());
    }
}
