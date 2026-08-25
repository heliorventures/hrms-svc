//! Ignored PostgreSQL contract tests for attendance adjustment locking and rollback.
//!
//! These tests are compile-checked by default and run only when explicitly selected with
//! `--ignored` and `KABIPAY_ATTENDANCE_TEST_DATABASE_URL` points to a disposable database.

use std::sync::Arc;

use anyhow::{ensure, Context, Result};
use chrono::{NaiveDate, NaiveTime};
use kabipay_attendance::attendance_management::{
    create_managed_attendance_segment_in_transaction, ManagedCreateCommand, SegmentTimes,
};
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait, TryGetable,
};
use tokio::sync::Barrier;
use uuid::Uuid;

const DATABASE_URL_ENV: &str = "KABIPAY_ATTENDANCE_TEST_DATABASE_URL";

struct Fixture {
    admin: DatabaseConnection,
    db: DatabaseConnection,
    schema: String,
    tenant_id: Uuid,
    employee_id: Uuid,
    actor_user_id: Uuid,
}

impl Fixture {
    async fn create() -> Result<Self> {
        let database_url = std::env::var(DATABASE_URL_ENV).with_context(|| {
            format!("{DATABASE_URL_ENV} must name a disposable PostgreSQL database")
        })?;
        let admin = Database::connect(&database_url).await?;
        let schema = format!("attendance_4b_{}", Uuid::new_v4().simple());
        admin
            .execute(Statement::from_string(
                DbBackend::Postgres,
                format!(r#"CREATE SCHEMA "{schema}""#),
            ))
            .await?;

        let mut options = ConnectOptions::new(database_url);
        options
            .max_connections(6)
            .min_connections(0)
            .sqlx_logging(false)
            .set_schema_search_path(format!(r#""{schema}",public"#));
        let db = Database::connect(options).await?;
        for ddl in [
            r#"CREATE TABLE "user" (
                id UUID PRIMARY KEY,
                tenant_id UUID NOT NULL
            )"#,
            r#"CREATE TABLE employee (
                id UUID PRIMARY KEY,
                tenant_id UUID NOT NULL
            )"#,
            r#"CREATE TABLE master_data (
                id UUID PRIMARY KEY,
                tenant_id UUID NOT NULL,
                category TEXT NOT NULL,
                data_key TEXT NOT NULL,
                value TEXT NOT NULL,
                description TEXT,
                display_order INTEGER,
                is_system BOOLEAN NOT NULL,
                is_active BOOLEAN NOT NULL,
                created_at TIMESTAMPTZ NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL
            )"#,
            r#"CREATE TABLE attendance (
                id UUID PRIMARY KEY,
                tenant_id UUID NOT NULL,
                employee_id UUID NOT NULL REFERENCES employee(id),
                shift_id UUID,
                work_date DATE NOT NULL,
                check_in_time TIME,
                check_out_time TIME,
                check_in_lat NUMERIC,
                check_in_lng NUMERIC,
                check_out_lat NUMERIC,
                check_out_lng NUMERIC,
                source TEXT,
                status TEXT,
                regularization_status TEXT,
                biometric_ref TEXT,
                overtime_hours NUMERIC,
                late_minutes INTEGER,
                early_exit_minutes INTEGER,
                created_at TIMESTAMPTZ NOT NULL,
                updated_at TIMESTAMPTZ NOT NULL
            )"#,
            r#"CREATE TABLE attendance_adjustment_audit (
                id UUID PRIMARY KEY,
                tenant_id UUID NOT NULL,
                attendance_id UUID NOT NULL REFERENCES attendance(id),
                target_employee_id UUID NOT NULL REFERENCES employee(id),
                actor_user_id UUID NOT NULL REFERENCES "user"(id),
                operation VARCHAR(10) NOT NULL CHECK (operation IN ('CREATE', 'UPDATE')),
                reason VARCHAR(500) NOT NULL,
                before_values JSONB,
                after_values JSONB NOT NULL,
                request_id VARCHAR(128),
                created_at TIMESTAMPTZ NOT NULL
            )"#,
        ] {
            db.execute(Statement::from_string(DbBackend::Postgres, ddl))
                .await?;
        }

        let tenant_id = Uuid::new_v4();
        let employee_id = Uuid::new_v4();
        let actor_user_id = Uuid::new_v4();
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "INSERT INTO employee (id, tenant_id) VALUES ($1, $2)",
            vec![employee_id.into(), tenant_id.into()],
        ))
        .await?;
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"INSERT INTO "user" (id, tenant_id) VALUES ($1, $2)"#,
            vec![actor_user_id.into(), tenant_id.into()],
        ))
        .await?;

        Ok(Self {
            admin,
            db,
            schema,
            tenant_id,
            employee_id,
            actor_user_id,
        })
    }

    fn command(
        &self,
        check_in_time: NaiveTime,
        check_out_time: NaiveTime,
        actor_user_id: Uuid,
    ) -> ManagedCreateCommand {
        ManagedCreateCommand {
            tenant_id: self.tenant_id,
            target_employee_id: self.employee_id,
            actor_user_id,
            segment: SegmentTimes {
                work_date: NaiveDate::from_ymd_opt(2026, 8, 20)
                    .expect("fixed harness date is valid"),
                check_in_time,
                check_out_time,
            },
            reason: "approved external harness adjustment".into(),
            request_id: Some(Uuid::new_v4().to_string()),
        }
    }

    async fn count(&self, table: &str) -> Result<i64> {
        ensure!(
            matches!(table, "attendance" | "attendance_adjustment_audit"),
            "unsupported harness table"
        );
        let row = self
            .db
            .query_one(Statement::from_string(
                DbBackend::Postgres,
                format!("SELECT COUNT(*) AS row_count FROM {table}"),
            ))
            .await?
            .context("count query returned no row")?;
        i64::try_get_by(&row, "row_count")
            .map_err(|error| anyhow::anyhow!("count conversion failed: {error:?}"))
    }

    async fn cleanup(self) -> Result<()> {
        let db_close = self.db.close().await.context("fixture connection close failed");
        let schema_drop = self
            .admin
            .execute(Statement::from_string(
                DbBackend::Postgres,
                format!(r#"DROP SCHEMA "{}" CASCADE"#, self.schema),
            ))
            .await
            .map(|_| ())
            .context("fixture schema drop failed");
        let admin_close = self
            .admin
            .close()
            .await
            .context("fixture admin connection close failed");

        let mut cleanup_error: Option<anyhow::Error> = None;
        for result in [db_close, schema_drop, admin_close] {
            if let Err(error) = result {
                cleanup_error = Some(match cleanup_error {
                    Some(primary) => primary.context(format!(
                        "an additional fixture cleanup operation failed: {error:#}"
                    )),
                    None => error,
                });
            }
        }
        match cleanup_error {
            Some(error) => Err(error),
            None => Ok(()),
        }
    }
}

async fn finish_with_cleanup<T>(fixture: Fixture, body_result: Result<T>) -> Result<T> {
    let cleanup_result = fixture.cleanup().await;
    match (body_result, cleanup_result) {
        (Ok(value), Ok(())) => Ok(value),
        (Ok(_), Err(cleanup_error)) => Err(cleanup_error),
        (Err(body_error), Ok(())) => Err(body_error),
        (Err(body_error), Err(cleanup_error)) => Err(body_error.context(format!(
            "test body failed and fixture cleanup also failed: {cleanup_error:#}"
        ))),
    }
}

async fn create_and_finish(
    db: DatabaseConnection,
    barrier: Arc<Barrier>,
    command: ManagedCreateCommand,
) -> KabiPayResult<()> {
    let mut txn = db.begin().await?;
    barrier.wait().await;
    match create_managed_attendance_segment_in_transaction(&mut txn, &command).await {
        Ok(_) => txn.commit().await.map_err(KabiPayError::from),
        Err(error) => {
            let _ = txn.rollback().await;
            Err(error)
        }
    }
}

fn time(hour: u32) -> NaiveTime {
    NaiveTime::from_hms_opt(hour, 0, 0).expect("fixed harness time is valid")
}

async fn concurrent_overlapping_managed_creates_body(fixture: &Fixture) -> Result<()> {
    let barrier = Arc::new(Barrier::new(2));
    let first = tokio::spawn(create_and_finish(
        fixture.db.clone(),
        Arc::clone(&barrier),
        fixture.command(time(9), time(17), fixture.actor_user_id),
    ));
    let second = tokio::spawn(create_and_finish(
        fixture.db.clone(),
        barrier,
        fixture.command(time(10), time(18), fixture.actor_user_id),
    ));
    let (first_result, second_result) = tokio::join!(first, second);
    let results = [first_result?, second_result?];
    let success_count = results.iter().filter(|result| result.is_ok()).count();
    let overlap_count = results
        .iter()
        .filter(|result| {
            matches!(
                result,
                Err(KabiPayError::Validation(message))
                    if message == "manual attendance overlaps with an existing segment for this day"
            )
        })
        .count();
    let attendance_count = fixture.count("attendance").await?;
    let audit_count = fixture.count("attendance_adjustment_audit").await?;
    ensure!(success_count == 1, "expected exactly one successful create");
    ensure!(overlap_count == 1, "expected exactly one overlap rejection");
    ensure!(attendance_count == 1, "expected exactly one attendance row");
    ensure!(audit_count == 1, "expected exactly one audit row");
    Ok(())
}

async fn audit_insert_failure_rolls_back_body(fixture: &Fixture) -> Result<()> {
    let mut txn = fixture.db.begin().await?;
    let result = create_managed_attendance_segment_in_transaction(
        &mut txn,
        &fixture.command(time(9), time(17), Uuid::new_v4()),
    )
    .await;
    let rollback_result = txn.rollback().await;
    ensure!(result.is_err(), "forced audit foreign-key failure must surface");
    rollback_result?;
    let attendance_count = fixture.count("attendance").await?;
    let audit_count = fixture.count("attendance_adjustment_audit").await?;
    ensure!(attendance_count == 0, "attendance write must roll back");
    ensure!(audit_count == 0, "failed audit must leave no audit row");
    Ok(())
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL approval"]
async fn concurrent_overlapping_managed_creates_commit_one_attendance_and_one_audit() -> Result<()> {
    let fixture = Fixture::create().await?;
    let body_result = concurrent_overlapping_managed_creates_body(&fixture).await;
    finish_with_cleanup(fixture, body_result).await
}

#[tokio::test]
#[ignore = "requires explicit disposable PostgreSQL approval"]
async fn audit_insert_failure_rolls_back_managed_attendance_write() -> Result<()> {
    let fixture = Fixture::create().await?;
    let body_result = audit_insert_failure_rolls_back_body(&fixture).await;
    finish_with_cleanup(fixture, body_result).await
}
