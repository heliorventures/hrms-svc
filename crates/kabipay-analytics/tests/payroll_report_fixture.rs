//! Runs only against the disposable native-import acceptance database.
#[path = "../src/entities/mod.rs"] mod entities;
#[path = "../src/services/mod.rs"] mod services;
#[path = "../src/resolvers/mod.rs"] mod resolvers;

#[tokio::test]
#[ignore = "requires the disposable integration fixture"]
async fn finalized_statements_appear_once_in_unpaid_report() {
    use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
    use services::hr_reports::{load, ReportFilter};
    let url = std::env::var("HRMS_IMPORT_TEST_DATABASE_URL").expect("fixture URL required");
    assert!(url.starts_with("postgres://postgres@127.0.0.1:"));
    let mut options = ConnectOptions::new(url);
    options.set_schema_search_path("tenant_import_fixture,public").sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    let tenant = "10000000-0000-0000-0000-000000000001".parse::<uuid::Uuid>().unwrap();
    let claims = serde_json::from_value(serde_json::json!({"sub":uuid::Uuid::nil(),"iss":"test","exp":9999999999i64,"iat":0,"tenant_id":tenant,"permissions":["payroll:read"],"permission_scopes":{"payroll:read":"ALL"}})).unwrap();
    let filter = ReportFilter { from_date:"2026-09-01".parse().unwrap(),to_date:"2026-12-31".parse().unwrap(),employee_id:None,employee_search:None };
    let data = load(&db,tenant,&claims,resolvers::hr_report_types::HrReportKind::UnpaidLeave,&filter,kabipay_common::tenant_business_clock::TenantBusinessClock::from_name("Asia/Kolkata").unwrap()).await.unwrap();
    let stored = db.query_all(Statement::from_string(DbBackend::Postgres,"SELECT e.employee_code, to_char(make_date(c.year,c.month,1),'YYYY-MM') AS month,s.statement->>'lwp_days' AS days,s.statement->>'lwp_amount' AS amount FROM payslip_statement s JOIN payslip p ON p.id=s.payslip_id JOIN payroll_cycle c ON c.id=p.payroll_cycle_id JOIN employee e ON e.id=p.employee_id WHERE c.year=2026 AND c.month BETWEEN 9 AND 12")).await.unwrap();
    assert!(!stored.is_empty());
    assert_eq!(data.rows.len(),stored.len());
    for row in stored {
        let code:String=row.try_get("","employee_code").unwrap();
        let month:String=row.try_get("","month").unwrap();
        let matches:Vec<_>=data.rows.iter().filter(|r|r[0]==code&&r[2]==month).collect();
        assert_eq!(matches.len(),1);
        assert_eq!(matches[0][7],"GROSS");
        assert_eq!(matches[0][10],row.try_get::<String>("","days").unwrap());
        assert_eq!(matches[0][11],row.try_get::<String>("","amount").unwrap());
    }
    db.close().await.unwrap();
}
