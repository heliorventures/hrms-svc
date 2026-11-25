//! Shared test-only admission for Node-owned loopback PostgreSQL rehearsals.
//! Never accepts an application DSN, .env, pgpass or arbitrary role/database.
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, EntityTrait, Schema,
    Statement,
};
use serde::Deserialize;
use std::{path::PathBuf, time::Duration};
use uuid::Uuid;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OwnedEmailFixture {
    port: u16,
    system_id: String,
    data_directory: PathBuf,
    database: String,
    #[serde(default)]
    metadata_database: Option<String>,
}
impl OwnedEmailFixture {
    pub fn load() -> Self {
        let path = PathBuf::from(
            std::env::var_os("KABIPAY_EMAIL_WRITERS_OWNED_FIXTURE")
                .expect("launch through owned Node fixture"),
        );
        assert!(path.is_absolute() && std::fs::metadata(&path).unwrap().len() < 16384);
        let fixture: Self = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        let data = fixture.data_directory.canonicalize().unwrap();
        assert_eq!(
            data.parent().unwrap(),
            path.parent().unwrap().canonicalize().unwrap()
        );
        assert!(data
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy()
            .starts_with("hrms-shared-grants-"));
        let pid = std::fs::read_to_string(data.join("postmaster.pid")).unwrap();
        let lines: Vec<_> = pid.lines().collect();
        assert_eq!(PathBuf::from(lines[1]).canonicalize().unwrap(), data);
        assert_eq!(lines[3].parse::<u16>().unwrap(), fixture.port);
        assert!(fixture.port > 1024 && fixture.port != 5432 && fixture.port != 5433);
        assert!(matches!(
            fixture.database.as_str(),
            "company_email_expense_fixture"
                | "company_email_leave_fixture"
                | "company_email_tax_fixture"
                | "company_email_profile_fixture"
                | "company_email_loan_fixture"
                | "company_email_attendance_fixture"
                | "company_email_timesheet_fixture"
                | "company_email_payroll_fixture"
                | "company_email_performance_fixture"
                | "company_email_delivery_fixture"
        ));
        assert!(fixture
            .metadata_database
            .as_deref()
            .is_none_or(|name| name == "email_metadata_fixture"));
        fixture
    }
    pub async fn metadata(&self) -> DatabaseConnection {
        let database = self
            .metadata_database
            .as_deref()
            .expect("owned metadata fixture required");
        assert_eq!(database, "email_metadata_fixture");
        self.connect_database("fixture_owner", database).await
    }
    pub async fn connect(&self, user: &str) -> DatabaseConnection {
        self.connect_database(user, &self.database).await
    }
    async fn connect_database(&self, user: &str, database: &str) -> DatabaseConnection {
        assert!(
            database == self.database
                || (Some(database) == self.metadata_database.as_deref() && user == "fixture_owner")
        );
        assert!(matches!(
            user,
            "fixture_owner"
                | "hrms_company_expense"
                | "hrms_company_leave"
                | "hrms_company_tax"
                | "hrms_company_employee"
                | "hrms_company_payroll"
                | "hrms_company_attendance"
                | "hrms_company_maintenance"
                | "hrms_company_performance"
                | "hrms_company_notification"
                | "hrms_fixture_metadata_leave"
        ));
        let mut options = ConnectOptions::new(format!(
            "postgres://{user}:synthetic-unused@127.0.0.1:{}/{}?sslmode=disable",
            self.port, database
        ));
        options
            .max_connections(2)
            .min_connections(1)
            .connect_timeout(Duration::from_secs(5))
            .sqlx_logging(false)
            .set_schema_search_path(if database == self.database {
                "tenant_fixture,pg_catalog"
            } else {
                "kabipay_ops,pg_catalog"
            });
        let db = Database::connect(options).await.unwrap();
        if user == "fixture_owner" {
            let system: String = db
                .query_one(Statement::from_string(
                    DbBackend::Postgres,
                    "SELECT system_identifier::text AS id FROM pg_control_system()".to_owned(),
                ))
                .await
                .unwrap()
                .unwrap()
                .try_get("", "id")
                .unwrap();
            assert_eq!(system, self.system_id);
        }
        db
    }
    pub async fn install_owned_sql(&self, owner: &DatabaseConnection) {
        let path = self
            .data_directory
            .parent()
            .unwrap()
            .join("owned-email-writers-setup.sql");
        assert!(std::fs::metadata(&path).unwrap().len() < 2_000_000);
        execute(owner, &std::fs::read_to_string(path).unwrap()).await;
    }
}
pub fn id(n: u32) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-{n:012}")).unwrap()
}
pub async fn execute(owner: &DatabaseConnection, sql: &str) {
    owner.execute_unprepared(sql).await.unwrap();
}
pub async fn entity<E: EntityTrait>(owner: &DatabaseConnection, entity: E) {
    let mut table = Schema::new(DbBackend::Postgres).create_table_from_entity(entity);
    table.if_not_exists();
    owner
        .execute(DbBackend::Postgres.build(&table))
        .await
        .unwrap();
}
/// Synthetic baseline defaults only, before the real mail DDL is installed.
/// They make unrelated required ORM profile fields deterministic test values;
/// production schema defaults and constraints are not modified by this helper.
pub async fn synthetic_defaults(owner: &DatabaseConnection) {
    execute(owner,r#"DO $$ DECLARE c record; v text; BEGIN
 FOR c IN SELECT table_name,column_name,data_type FROM information_schema.columns
 WHERE table_schema='tenant_fixture' AND is_nullable='NO' AND column_default IS NULL LOOP
 v:=CASE c.data_type WHEN 'uuid' THEN 'gen_random_uuid()' WHEN 'boolean' THEN 'false'
 WHEN 'text' THEN quote_literal('fixture') WHEN 'character varying' THEN quote_literal('fixture')
 WHEN 'integer' THEN '0' WHEN 'bigint' THEN '0' WHEN 'smallint' THEN '0' WHEN 'numeric' THEN '0'
 WHEN 'timestamp with time zone' THEN 'clock_timestamp()' WHEN 'timestamp without time zone' THEN 'clock_timestamp()'
 WHEN 'date' THEN 'CURRENT_DATE' WHEN 'time without time zone' THEN quote_literal('00:00')
 WHEN 'jsonb' THEN quote_literal('{}')||'::jsonb' WHEN 'json' THEN quote_literal('{}')||'::json' ELSE NULL END;
 IF v IS NOT NULL THEN EXECUTE format('ALTER TABLE tenant_fixture.%I ALTER COLUMN %I SET DEFAULT %s',c.table_name,c.column_name,v); END IF;
 END LOOP; END $$"#).await;
}
pub async fn count(owner: &DatabaseConnection, table: &str) -> i64 {
    assert!(matches!(
        table,
        "expense"
            | "travel_request"
            | "workflow_instance"
            | "workflow_action"
            | "important_email_intent"
            | "notification"
            | "leave_request"
            | "company_event_outbox"
    ));
    owner
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            format!("SELECT count(*)::bigint AS n FROM {table}"),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap()
}
pub async fn inject_intent_failure(owner: &DatabaseConnection, enabled: bool) {
    execute(owner,if enabled{"CREATE FUNCTION fixture_reject_mail() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN RAISE EXCEPTION 'synthetic intent failure'; END $$;CREATE TRIGGER fixture_reject_mail BEFORE INSERT ON important_email_intent FOR EACH ROW EXECUTE FUNCTION fixture_reject_mail()"}else{"DROP TRIGGER fixture_reject_mail ON important_email_intent;DROP FUNCTION fixture_reject_mail()"}).await;
}
pub async fn transaction_counts(owner: &DatabaseConnection) -> (i64, i64, i64) {
    (
        count(owner, "workflow_action").await,
        count(owner, "important_email_intent").await,
        count(owner, "company_event_outbox").await,
    )
}
