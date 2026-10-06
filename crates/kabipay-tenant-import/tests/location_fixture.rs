//! Only the disposable import fixture may execute this transactional persistence check.
use chrono::NaiveDate;
use kabipay_tenant_import::location_import::{save, LocationInput};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires the disposable integration fixture with migration 0097"]
async fn imported_location_persists_both_links_and_replays_without_new_history() {
    let url = std::env::var("HRMS_IMPORT_TEST_DATABASE_URL").expect("fixture-only URL required");
    assert!(url.starts_with("postgres://postgres@127.0.0.1:"));
    let mut options = ConnectOptions::new(url);
    options
        .set_schema_search_path("tenant_import_fixture,public")
        .sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    let txn = db.begin().await.unwrap();
    let tenant = Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap();
    let actor = Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap();
    let employee = Uuid::new_v4();
    let today: NaiveDate = "2026-10-07".parse().unwrap();
    kabipay_common::working_calendar::lock_calendar(&txn, tenant)
        .await
        .unwrap();
    txn.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee(id,tenant_id,employee_code,first_name,last_name,date_of_joining,status) VALUES($1,$2,$3,'Location','Fixture','2020-01-01','ACTIVE')",
        [employee.into(), tenant.into(), format!("LOCATION-{employee}").into()])).await.unwrap();
    let input = LocationInput {
        name: format!("Fixture Office {employee}"),
        effective_from: today,
    };
    assert_eq!(
        save(&txn, tenant, actor, employee, &input, today)
            .await
            .unwrap(),
        "CREATED"
    );
    assert_eq!(
        save(&txn, tenant, actor, employee, &input, today)
            .await
            .unwrap(),
        "UNCHANGED"
    );
    let row = txn.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*)::bigint AS n, MIN(a.revision)::bigint AS revision FROM employee e JOIN employee_location_assignment a ON a.tenant_id=e.tenant_id AND a.employee_id=e.id AND a.location_id=e.location_id JOIN location l ON l.tenant_id=e.tenant_id AND l.id=e.location_id WHERE e.tenant_id=$1 AND e.id=$2",
        [tenant.into(), employee.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i64>("", "n").unwrap(), 1);
    assert_eq!(row.try_get::<i64>("", "revision").unwrap(), 1);
    let other = LocationInput {
        name: format!("Other {employee}"),
        ..input
    };
    assert!(save(&txn, tenant, actor, employee, &other, today)
        .await
        .is_err());
    txn.rollback().await.unwrap();
}
