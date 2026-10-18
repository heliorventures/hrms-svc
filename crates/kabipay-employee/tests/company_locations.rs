//! Uses only the disposable database started by scripts/test-company-locations.ps1.
use chrono::Utc;
use kabipay_employee::services::company_location_repository::{
    self as locations, SaveLocationCommand,
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement, Value,
};
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
}

impl Fixture {
    async fn new() -> Self {
        let url = std::env::var("COMPANY_LOCATION_TEST_DATABASE_URL")
            .expect("use scripts/test-company-locations.ps1");
        assert!(
            url.starts_with("postgresql://location_fixture:location_fixture@127.0.0.1:")
                && url.ends_with("/company_location_regression"),
            "refusing a non-fixture database"
        );
        let schema = format!("location_fixture_{}", Uuid::new_v4().simple());
        let admin = Database::connect(url.clone()).await.unwrap();
        admin
            .execute_unprepared(&format!("CREATE SCHEMA {schema}"))
            .await
            .unwrap();
        let mut options = ConnectOptions::new(url);
        options
            .set_schema_search_path(format!("{schema},public"))
            .sqlx_logging(false);
        let db = Database::connect(options).await.unwrap();
        // Match 0006-003-create-location, 0027-003-create-audit-log and the
        // 0097 normalized active-name index, including the real audit user FK.
        db.execute_unprepared(include_str!("support/company_locations.sql"))
            .await
            .unwrap();
        let fixture = Self {
            db,
            tenant: Uuid::new_v4(),
            actor: Uuid::new_v4(),
        };
        fixture
            .sql(
                "INSERT INTO \"user\"(id,tenant_id) VALUES($1,$2)",
                vec![fixture.actor.into(), fixture.tenant.into()],
            )
            .await;
        admin.close().await.unwrap();
        fixture
    }

    async fn sql(&self, sql: &str, values: Vec<Value>) {
        self.db
            .execute(Statement::from_sql_and_values(
                DbBackend::Postgres,
                sql,
                values,
            ))
            .await
            .unwrap();
    }

    async fn count(&self, table: &str) -> i64 {
        assert!(matches!(table, "location" | "audit_log"));
        self.db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                format!("SELECT count(*) AS amount FROM {table} WHERE tenant_id=$1"),
                [self.tenant.into()],
            ))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "amount")
            .unwrap()
    }

    fn command(&self, name: &str) -> SaveLocationCommand {
        SaveLocationCommand {
            id: None,
            expected_updated_at: None,
            name: name.into(),
            address: Some("Pune".into()),
            city: Some("Pune".into()),
            state: Some("Maharashtra".into()),
            country: Some("India".into()),
        }
    }
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn company_location_create_update_and_audit_round_trip() {
    let f = Fixture::new().await;
    let created = locations::save_location(&f.db, f.tenant, f.actor, f.command("Pune"))
        .await
        .unwrap();
    assert_eq!(created.name, "Pune");
    assert_eq!(created.address.as_deref(), Some("Pune"));
    assert_eq!(created.city.as_deref(), Some("Pune"));
    assert_eq!(created.state.as_deref(), Some("Maharashtra"));
    assert_eq!(created.country.as_deref(), Some("India"));
    assert_eq!(f.count("audit_log").await, 1);
    let mut update = f.command("  Pune   Office  ");
    update.id = Some(created.id);
    update.expected_updated_at = Some(created.updated_at);
    let updated = locations::save_location(&f.db, f.tenant, f.actor, update)
        .await
        .unwrap();
    assert_eq!(updated.id, created.id);
    assert_eq!(updated.name, "Pune Office");
    let mut stale = f.command("Outdated update");
    stale.id = Some(created.id);
    stale.expected_updated_at = Some(created.updated_at);
    assert!(matches!(
        locations::save_location(&f.db, f.tenant, f.actor, stale).await,
        Err(kabipay_common::KabiPayError::Validation(_))
    ));
    assert_eq!(f.count("location").await, 1);
    assert_eq!(f.count("audit_log").await, 2);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn company_location_search_is_bound_escaped_and_tenant_scoped() {
    let f = Fixture::new().await;
    for (tenant, name, deleted) in [
        (f.tenant, "Pune", false),
        (f.tenant, "100%_\\ Office", false),
        (f.tenant, "100AB_\\ Office", false),
        (f.tenant, "100%X\\ Office", false),
        (f.tenant, "100AB\\ Office", false),
        (f.tenant, "Pune retired", true),
        (Uuid::new_v4(), "Pune other tenant", false),
    ] {
        f.sql("INSERT INTO location(id,tenant_id,name,is_deleted,created_at,updated_at) VALUES($1,$2,$3,$4,$5,$5)",
            vec![Uuid::new_v4().into(),tenant.into(),name.into(),deleted.into(),Utc::now().into()]).await;
    }
    let page = kabipay_common::PageInput {
        page: 1,
        per_page: 25,
    };
    for (search, expected) in [
        ("pune", 1),
        ("%_\\", 1),
        ("%", 2),
        ("_", 2),
        ("\\", 4),
        ("%' OR TRUE --", 0),
    ] {
        let (rows, total) = locations::list(&f.db, f.tenant, page, Some(search.into()), true)
            .await
            .unwrap();
        assert_eq!(total, expected, "search={search}");
        assert_eq!(rows.len() as u64, expected);
        assert!(rows
            .iter()
            .all(|row| row.tenant_id == f.tenant && !row.is_deleted));
    }
    let (_, total) = locations::list(&f.db, f.tenant, page, Some("Pune".into()), false)
        .await
        .unwrap();
    assert_eq!(total, 2);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn company_location_duplicate_names_and_audit_failure_roll_back() {
    let f = Fixture::new().await;
    locations::save_location(&f.db, f.tenant, f.actor, f.command("Pune Office"))
        .await
        .unwrap();
    let duplicate =
        locations::save_location(&f.db, f.tenant, f.actor, f.command("  pune   OFFICE ")).await;
    assert!(matches!(
        duplicate,
        Err(kabipay_common::KabiPayError::Validation(_))
    ));
    assert_eq!(f.count("location").await, 1);
    assert_eq!(f.count("audit_log").await, 1);
    let invalid_actor =
        locations::save_location(&f.db, f.tenant, Uuid::new_v4(), f.command("Mumbai")).await;
    assert!(matches!(
        invalid_actor,
        Err(kabipay_common::KabiPayError::Database(_))
    ));
    assert_eq!(
        f.count("location").await,
        1,
        "failed audit must roll back the location"
    );
    assert_eq!(f.count("audit_log").await, 1);
}
