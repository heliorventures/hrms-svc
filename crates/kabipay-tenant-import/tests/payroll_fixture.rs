//! Invoked explicitly by the disposable Node fixture; never consumes repository .env.
use kabipay_payroll::services::{payroll_period_input, payroll_service, payslip_presentation};
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;
#[tokio::test]
#[ignore = "requires the disposable integration fixture"]
async fn imported_month_uses_the_normal_pay_run_and_immutable_statement() {
    let url = std::env::var("HRMS_IMPORT_TEST_DATABASE_URL").expect("fixture-only URL required");
    assert!(url.starts_with("postgres://postgres@127.0.0.1:"));
    let mut options = ConnectOptions::new(url);
    options
        .set_schema_search_path("tenant_import_fixture,public")
        .sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    let tenant = Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap();
    let actor = Uuid::parse_str("10000000-0000-0000-0000-000000000002").unwrap();
    let employee = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT id FROM employee WHERE employee_code='EXAMPLE-001'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<Uuid>("", "id")
        .unwrap();
    let input = payroll_period_input::find(&db, tenant, employee, 2026, 9)
        .await
        .unwrap()
        .unwrap();
    assert!(input.ready);
    let source: kabipay_payroll::services::payroll_rules::PeriodInput =
        serde_json::from_value(input.input.clone()).unwrap();
    verify_financial_review_guards(&db, tenant, actor, employee).await;
    verify_midmonth_rule_guard(&db, tenant, employee).await;
    let expected = kabipay_payroll::services::payroll_rules::calculate_period(&source).unwrap();
    let cycle = payroll_service::create_payroll_cycle(
        &db,
        tenant,
        "Fictional September".into(),
        9,
        2026,
        None,
    )
    .await
    .unwrap();
    let result = payroll_service::run_payroll_for_cycle(&db, tenant, cycle.id, actor)
        .await
        .unwrap();
    assert_eq!(result.status, "PROCESSED");
    let slips = payroll_service::list_payslips(&db, tenant, Some(employee), 10)
        .await
        .unwrap();
    assert_eq!(slips.len(), 1);
    let slip = &slips[0];
    assert_eq!(slip.gross_salary, expected.gross + expected.incentive);
    assert_eq!(slip.total_deductions, expected.total_deductions);
    assert_eq!(slip.net_salary, expected.net_earned);
    let statement = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT statement FROM payslip_statement WHERE tenant_id=$1 AND payslip_id=$2",
            [tenant.into(), slip.id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<serde_json::Value>("", "statement")
        .unwrap();
    assert_eq!(
        statement["remaining_payable"],
        expected.remaining_payable.to_string()
    );
    assert_eq!(statement["advance_already_paid"], "5000.00");
    let lines = payroll_service::payslip_lines_by_payslip_ids(&db, tenant, &[slip.id])
        .await
        .unwrap();
    let presentation = payslip_presentation::load(&db, tenant, slip, &lines[&slip.id])
        .await
        .unwrap();
    assert!(!presentation
        .lines
        .iter()
        .any(|line| line.component_type == "EMPLOYER_CONTRIBUTION"));
    let basic = presentation
        .lines
        .iter()
        .find(|line| line.code == "BASIC")
        .unwrap();
    let basic_id = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT id FROM salary_component WHERE code='BASIC'",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<Uuid>("", "id")
        .unwrap();
    kabipay_payroll::services::component_display::save(&db, tenant, basic_id, false)
        .await
        .unwrap();
    let hidden = payslip_presentation::load(&db, tenant, slip, &lines[&slip.id])
        .await
        .unwrap();
    assert!(!hidden.lines.iter().any(|line| line.id == basic.id));
    let unchanged = payroll_service::list_payslips(&db, tenant, Some(employee), 10)
        .await
        .unwrap();
    assert_eq!(unchanged[0].net_salary, expected.net_earned);
    let bank_csv = payroll_service::payroll_bank_transfer_csv(&db, tenant, 9, 2026)
        .await
        .unwrap();
    let amount = bank_csv.lines().nth(1).unwrap().split(',').nth(8).unwrap();
    assert_eq!(
        amount.parse::<rust_decimal::Decimal>().unwrap(),
        expected.remaining_payable,
        "transfers must not pay an advance twice"
    );
    assert!(
        payroll_service::run_payroll_for_cycle(&db, tenant, cycle.id, actor)
            .await
            .is_err()
    );
    let transaction = db.begin().await.unwrap();
    assert!(payroll_period_input::save(
        &transaction,
        tenant,
        actor,
        employee,
        source,
        None,
        Some(input.revision)
    )
    .await
    .is_err());
    transaction.rollback().await.unwrap();
    verify_reviewed_dated_lwp(&db, tenant, actor, employee).await;
    db.close().await.unwrap();
}

async fn verify_midmonth_rule_guard(
    db: &sea_orm::DatabaseConnection,
    tenant: Uuid,
    source_employee: Uuid,
) {
    use kabipay_employee::services::employee_service::{self, NewEmployee};
    let transaction = db.begin().await.unwrap();
    let midmonth = employee_service::create(
        &transaction,
        tenant,
        NewEmployee {
            employee_code: "MIDMONTH-REVIEW".into(),
            first_name: "Fictional".into(),
            last_name: "Midmonth".into(),
            date_of_joining: chrono::NaiveDate::from_ymd_opt(2026, 9, 15).unwrap(),
            department_id: None,
            designation_id: None,
            reporting_manager_id: None,
            employment_type: None,
            status: "ACTIVE".into(),
            user_id: None,
        },
    )
    .await
    .unwrap();
    transaction.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_payroll_rule(tenant_id,employee_id,effective_from,rules,updated_by) SELECT tenant_id,$2,'2026-09-15',rules,updated_by FROM employee_payroll_rule WHERE tenant_id=$1 AND employee_id=$3 LIMIT 1",
        [tenant.into(),midmonth.id.into(),source_employee.into()])).await.unwrap();
    let result = kabipay_payroll::services::imported_payroll::run(
        &transaction,
        tenant,
        Uuid::new_v4(),
        &midmonth,
        chrono::NaiveDate::from_ymd_opt(2026, 9, 1).unwrap(),
    )
    .await;
    assert!(
        result.is_err(),
        "midmonth imported employee must never enter legacy payroll without reviewed period inputs"
    );
    transaction.rollback().await.unwrap();
}

async fn verify_reviewed_dated_lwp(
    db: &sea_orm::DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
) {
    let request = Uuid::new_v4();
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO leave_request(id,tenant_id,employee_id,leave_type_id,from_date,to_date,days_requested,status,approved_by) SELECT $1,$2,$3,id,'2026-10-01','2026-10-01',1,'APPROVED',$4 FROM leave_type WHERE tenant_id=$2 AND code='LWP'",
        [request.into(),tenant.into(),employee.into(),actor.into()])).await.unwrap();
    let package: kabipay_tenant_import::contract::ImportPackage = serde_json::from_str(
        include_str!("../../../../hrms-database/import-templates/v1/example.synthetic.json"),
    )
    .unwrap();
    let mut input = package.employees[0].period_input.clone().unwrap();
    input.month = 10;
    input.gross_rule = "FIXED_MINUS_LWP".into();
    input.earned_gross_override = None;
    input.month_days = Some("31".into());
    input.paid_days = Some("30".into());
    input.present_days = Some("30".into());
    input.lwp_days = Some("1".into());
    input.source_lwp_days = Some("1".into());
    input.lwp_amount_override = None;
    input.expected_statement.clear();
    input.expected_earned_components=serde_json::from_value(serde_json::json!({"BASIC":"14516.13","HRA":"7258.07","CONVEYANCE":"3629.03","OTHER":"3629.03"})).unwrap();
    let transaction = db.begin().await.unwrap();
    let draft = payroll_period_input::save(
        &transaction,
        tenant,
        actor,
        employee,
        input.clone(),
        None,
        None,
    )
    .await
    .unwrap();
    assert!(
        !draft.ready,
        "unreviewed approved LWP must stage monthly payroll"
    );
    let review = kabipay_payroll::services::imported_lwp::review(
        &transaction,
        tenant,
        employee,
        2026,
        10,
        false,
    )
    .await
    .unwrap();
    assert_eq!(review["days"], "1");
    input.approved_lwp_review_hash = review["hash"].as_str().map(str::to_string);
    let ready = payroll_period_input::save(
        &transaction,
        tenant,
        actor,
        employee,
        input.clone(),
        None,
        Some(draft.revision),
    )
    .await
    .unwrap();
    assert!(ready.ready);
    transaction.commit().await.unwrap();
    let frozen = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT COUNT(*)::bigint AS n FROM payroll_unpaid_leave_allocation",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i64>("", "n")
        .unwrap();
    assert_eq!(frozen, 0, "HR review must not consume dated leave");
    let cycle = payroll_service::create_payroll_cycle(
        db,
        tenant,
        "Fictional October".into(),
        10,
        2026,
        None,
    )
    .await
    .unwrap();
    payroll_service::run_payroll_for_cycle(db, tenant, cycle.id, actor)
        .await
        .unwrap();
    let expected = kabipay_payroll::services::payroll_rules::calculate_period(&input).unwrap();
    let slips = payroll_service::list_payslips(db, tenant, Some(employee), 10)
        .await
        .unwrap();
    let slip = slips
        .iter()
        .find(|slip| slip.payroll_cycle_id == cycle.id)
        .unwrap();
    assert_eq!(slip.gross_salary, expected.gross + expected.incentive);
    assert_eq!(
        slip.total_deductions, expected.total_deductions,
        "LWP must not be charged a second time as a deduction"
    );
    let statement = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT statement FROM payslip_statement WHERE tenant_id=$1 AND payslip_id=$2",
            [tenant.into(), slip.id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<serde_json::Value>("", "statement")
        .unwrap();
    assert_eq!(statement["dated_lwp"]["days"], "1");
    let historical = db
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT historical_lwp FROM leave_import_history",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<rust_decimal::Decimal>("", "historical_lwp")
        .unwrap();
    assert_eq!(historical, rust_decimal::Decimal::from(2));
}

async fn verify_financial_review_guards(
    db: &sea_orm::DatabaseConnection,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
) {
    let package: kabipay_tenant_import::contract::ImportPackage = serde_json::from_str(
        include_str!("../../../../hrms-database/import-templates/v1/example.synthetic.json"),
    )
    .unwrap();
    let source = &package.employees[0];
    let transaction = db.begin().await.unwrap();
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE salary_structure_component SET calculation_value=calculation_value+1",
        ))
        .await
        .unwrap();
    let salary_guard = kabipay_tenant_import::salary_import::salary(
        &transaction,
        tenant,
        actor,
        employee,
        package.salary_start(source).unwrap(),
        source.recurring_salary.as_ref().unwrap(),
    )
    .await
    .is_err();
    transaction.rollback().await.unwrap();
    let transaction = db.begin().await.unwrap();
    transaction
        .execute(Statement::from_string(
            DbBackend::Postgres,
            "UPDATE leave_balance SET used_days=used_days+1",
        ))
        .await
        .unwrap();
    let paid = transaction
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT leave_type_id FROM leave_import_history",
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<Uuid>("", "leave_type_id")
        .unwrap();
    let mut opening: kabipay_leave::services::leave_import_history::OpeningSnapshot =
        serde_json::from_value(source.leave_opening.clone().unwrap()).unwrap();
    opening.as_of = opening.as_of.pred_opt().unwrap();
    let history = kabipay_leave::services::leave_import_history::validate_opening(&opening)
        .unwrap()
        .historical_lwp;
    let leave_guard = kabipay_leave::services::leave_import_history::save(
        &transaction,
        tenant,
        actor,
        employee,
        paid,
        opening,
        history,
        serde_json::json!({}),
    )
    .await
    .is_err();
    transaction.rollback().await.unwrap();
    assert!(salary_guard && leave_guard,"review guards: changed immutable salary={salary_guard}, manually changed leave balance={leave_guard}");
}
