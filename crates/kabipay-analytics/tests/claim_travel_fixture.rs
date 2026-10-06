//! User-run acceptance against a disposable localhost PostgreSQL database only.
#[path = "../src/entities/mod.rs"]
mod entities;
#[path = "../src/resolvers/mod.rs"]
mod resolvers;
#[path = "../src/services/mod.rs"]
mod services;

use kabipay_common::tenant_business_clock::TenantBusinessClock;
use resolvers::hr_report_types::{ClaimTravelReportFilterInput, HrReportKind};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
use services::claim_travel_report_filters::ClaimTravelFilter;
use services::claim_travel_reports::{load_csv, load_page};
use services::hr_reports::ReportFilter;
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires a disposable localhost PostgreSQL fixture; run manually"]
async fn claim_travel_filters_pages_csv_overlap_and_foreign_ids() {
    let url = std::env::var("HRMS_IMPORT_TEST_DATABASE_URL").expect("fixture URL required");
    assert!(
        url.starts_with("postgres://postgres@127.0.0.1:"),
        "only the disposable localhost fixture is allowed"
    );
    let schema = format!("claim_travel_fixture_{}", Uuid::new_v4().simple());
    let mut options = ConnectOptions::new(url);
    options
        .set_schema_search_path(format!("{schema},public"))
        .sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    db.execute_unprepared(&format!("CREATE SCHEMA {schema}"))
        .await
        .unwrap();
    db.execute_unprepared(&format!(r#"
      CREATE TABLE {schema}.employee (id uuid PRIMARY KEY,tenant_id uuid,employee_code text,first_name text,last_name text,department_id uuid,location_id uuid,is_deleted boolean DEFAULT false);
      CREATE TABLE {schema}.department (id uuid PRIMARY KEY,tenant_id uuid,name text,is_deleted boolean DEFAULT false);
      CREATE TABLE {schema}.location (id uuid PRIMARY KEY,tenant_id uuid,name text,is_deleted boolean DEFAULT false);
      CREATE TABLE {schema}.expense_category (id uuid PRIMARY KEY,tenant_id uuid,name text,is_deleted boolean DEFAULT false);
      CREATE TABLE {schema}.travel_request (id uuid PRIMARY KEY,tenant_id uuid,employee_id uuid,origin_location text,destination_location text,from_date date,to_date date,purpose text,estimated_amount numeric,currency text,status text,submitted_at timestamptz,supporting_file_storage_id uuid);
      CREATE TABLE {schema}.expense (id uuid PRIMARY KEY,tenant_id uuid,employee_id uuid,expense_category_id uuid,travel_request_id uuid,expense_date date,submitted_at timestamptz,title text,amount numeric,approved_amount numeric,currency text,status text,payment_status text,payment_reference text,receipt_file_storage_id uuid,is_deleted boolean DEFAULT false);
    "#)).await.unwrap();
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let department = Uuid::new_v4();
    let location = Uuid::new_v4();
    let category = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "INSERT INTO employee(id,tenant_id,employee_code,first_name,last_name,department_id,location_id) VALUES($1,$2,'EMP_10%','Asha','Rao',$3,$4)", vec![employee.into(),tenant.into(),department.into(),location.into()])).await.unwrap();
    for (table, id) in [
        ("department", department),
        ("location", location),
        ("expense_category", category),
    ] {
        db.execute(Statement::from_sql_and_values(
            DbBackend::Postgres,
            format!("INSERT INTO {table}(id,tenant_id,name) VALUES($1,$2,'Fixture')"),
            vec![id.into(), tenant.into()],
        ))
        .await
        .unwrap();
    }
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "INSERT INTO expense(id,tenant_id,employee_id,expense_category_id,expense_date,submitted_at,title,amount,approved_amount,currency,status,payment_status) SELECT md5('claim-'||n)::uuid,$1,$2,$3,'2026-10-06','2026-10-06 10:00:00+00','=SUM(1,2)',12345678901234567890.01,NULL,CASE WHEN n%2=0 THEN 'INR' ELSE 'USD' END,'PENDING','NONE' FROM generate_series(1,125) n", vec![tenant.into(),employee.into(),category.into()])).await.unwrap();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "INSERT INTO travel_request(id,tenant_id,employee_id,origin_location,destination_location,from_date,to_date,purpose,estimated_amount,currency,status,submitted_at) VALUES($1,$2,$3,'Pune','Mumbai','2026-09-20','2026-11-02','Boundary trip',100.01,'INR','APPROVED','2026-09-19 10:00:00+00')", vec![Uuid::new_v4().into(),tenant.into(),employee.into()])).await.unwrap();
    let claims = |permission: &str| {
        serde_json::from_value(serde_json::json!({"sub":Uuid::nil(),"iss":"test","exp":9999999999i64,"iat":0,"tenant_id":tenant,"permissions":[permission],"permission_scopes":{permission:"ALL"}})).unwrap()
    };
    let base = || ReportFilter {
        from_date: "2026-10-01".parse().unwrap(),
        to_date: "2026-10-31".parse().unwrap(),
        employee_id: None,
        employee_search: Some("EMP_10%".into()),
    };
    let filter = ClaimTravelFilter::new(
        base(),
        Some(ClaimTravelReportFilterInput {
            department_id: Some(department.to_string().into()),
            location_id: Some(location.to_string().into()),
            expense_category_id: Some(category.to_string().into()),
            approval_status: Some("PENDING".into()),
            payment_status: Some("NONE".into()),
            ..Default::default()
        }),
        HrReportKind::ExpenseClaims,
    )
    .unwrap();
    let clock = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
    let mut combined = Vec::new();
    for (offset, expected) in [(0, 50), (50, 50), (100, 25)] {
        let page = load_page(
            &db,
            tenant,
            &claims("expense:read"),
            HrReportKind::ExpenseClaims,
            &filter,
            offset,
            50,
            clock,
        )
        .await
        .unwrap();
        assert_eq!(page.total_rows, 125);
        assert_eq!(page.rows.len(), expected);
        assert_eq!(page.rows[0][8], "12345678901234567890.01");
        assert_eq!(page.rows[0][9], "");
        combined.extend(page.rows);
    }
    let csv = load_csv(
        &db,
        tenant,
        &claims("expense:read"),
        HrReportKind::ExpenseClaims,
        &filter,
        clock,
    )
    .await
    .unwrap();
    assert_eq!(csv.row_count, 125);
    let columns = load_page(
        &db,
        tenant,
        &claims("expense:read"),
        HrReportKind::ExpenseClaims,
        &filter,
        0,
        50,
        clock,
    )
    .await
    .unwrap()
    .columns;
    assert_eq!(
        csv.csv,
        services::hr_reports::render_csv(&columns, &combined)
    );
    let travel = ClaimTravelFilter::new(base(), None, HrReportKind::TravelRequests).unwrap();
    assert_eq!(
        load_page(
            &db,
            tenant,
            &claims("travel:read"),
            HrReportKind::TravelRequests,
            &travel,
            0,
            50,
            clock
        )
        .await
        .unwrap()
        .total_rows,
        1
    );
    let foreign_location = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO location(id,tenant_id,name) VALUES($1,$2,'Foreign company')",
        vec![foreign_location.into(), Uuid::new_v4().into()],
    ))
    .await
    .unwrap();
    let foreign = ClaimTravelFilter::new(
        base(),
        Some(ClaimTravelReportFilterInput {
            location_id: Some(foreign_location.to_string().into()),
            ..Default::default()
        }),
        HrReportKind::ExpenseClaims,
    )
    .unwrap();
    assert!(load_page(
        &db,
        tenant,
        &claims("expense:read"),
        HrReportKind::ExpenseClaims,
        &foreign,
        0,
        50,
        clock
    )
    .await
    .is_err());
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO expense(id,tenant_id,employee_id,expense_category_id,expense_date,submitted_at,title,amount,approved_amount,currency,status,payment_status) SELECT md5('claim-'||n)::uuid,$1,$2,$3,'2026-10-06','2026-10-06 10:00:00+00','Additional claim',100,NULL,'INR','PENDING','NONE' FROM generate_series(126,10001) n",vec![tenant.into(),employee.into(),category.into()])).await.unwrap();
    let rejected = load_csv(
        &db,
        tenant,
        &claims("expense:read"),
        HrReportKind::ExpenseClaims,
        &filter,
        clock,
    )
    .await;
    assert!(
        matches!(rejected, Err(kabipay_common::KabiPayError::Validation(message)) if message.contains("Narrow the filters"))
    );
    db.execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
    db.close().await.unwrap();
}
