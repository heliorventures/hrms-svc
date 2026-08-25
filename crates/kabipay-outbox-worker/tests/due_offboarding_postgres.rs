use chrono::{NaiveDate, Utc};
use kabipay_common::due_offboarding::process_due_separations;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
use uuid::Uuid;

async fn test_db() -> Option<sea_orm::DatabaseConnection> {
    let url = std::env::var("KABIPAY_TEST_DATABASE_URL").ok()?;
    let mut options = ConnectOptions::new(url);
    options.max_connections(1).min_connections(1);
    Some(Database::connect(options).await.expect("connect disposable PostgreSQL database"))
}

async fn create_fixture_tables(db: &sea_orm::DatabaseConnection) {
    db.execute_unprepared(
        r#"
CREATE TEMP TABLE separation (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    employee_id UUID NOT NULL,
    status VARCHAR(50) NOT NULL,
    last_working_date DATE NOT NULL,
    offboarded_at TIMESTAMPTZ,
    offboarding_event_id UUID,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TEMP TABLE employee (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    user_id UUID,
    status VARCHAR(50) NOT NULL,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TEMP TABLE "user" (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    is_active BOOLEAN NOT NULL,
    is_deleted BOOLEAN NOT NULL DEFAULT FALSE,
    updated_at TIMESTAMPTZ NOT NULL DEFAULT NOW()
);
CREATE TEMP TABLE user_session (id UUID PRIMARY KEY, user_id UUID NOT NULL);
CREATE TEMP TABLE outbox_event (
    id UUID PRIMARY KEY,
    tenant_id UUID NOT NULL,
    aggregate_type VARCHAR(100) NOT NULL,
    aggregate_id UUID NOT NULL,
    event_type VARCHAR(150) NOT NULL,
    payload JSONB NOT NULL,
    status VARCHAR(30) NOT NULL,
    retry_count INT NOT NULL,
    last_error TEXT,
    created_at TIMESTAMPTZ NOT NULL,
    processed_at TIMESTAMPTZ,
    claimed_at TIMESTAMPTZ
);
"#,
    )
    .await
    .expect("create temporary lifecycle tables");
}

async fn insert_case(
    db: &sea_orm::DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    user_id: Uuid,
    separation_id: Uuid,
    status: &str,
    last_working_date: NaiveDate,
) {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"INSERT INTO "user" (id, tenant_id, is_active) VALUES ($1, $2, TRUE)"#,
        vec![user_id.into(), tenant_id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO employee (id, tenant_id, user_id, status) VALUES ($1, $2, $3, 'ACTIVE')",
        vec![employee_id.into(), tenant_id.into(), user_id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO user_session (id, user_id) VALUES ($1, $2)",
        vec![Uuid::new_v4().into(), user_id.into()],
    ))
    .await
    .unwrap();
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        r#"INSERT INTO separation
           (id, tenant_id, employee_id, status, last_working_date, updated_at)
           VALUES ($1, $2, $3, $4, $5, $6)"#,
        vec![
            separation_id.into(),
            tenant_id.into(),
            employee_id.into(),
            status.into(),
            last_working_date.into(),
            Utc::now().into(),
        ],
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn due_offboarding_is_tenant_scoped_idempotent_and_status_aware() {
    let Some(db) = test_db().await else {
        eprintln!("skipping: KABIPAY_TEST_DATABASE_URL is not set");
        return;
    };
    create_fixture_tables(&db).await;
    let tenant_id = Uuid::new_v4();
    let today: NaiveDate = "2026-08-25".parse().unwrap();

    let due_employee = Uuid::new_v4();
    let due_user = Uuid::new_v4();
    let due_separation = Uuid::new_v4();
    insert_case(
        &db,
        tenant_id,
        due_employee,
        due_user,
        due_separation,
        "APPROVED",
        today,
    )
    .await;

    let future_employee = Uuid::new_v4();
    let future_user = Uuid::new_v4();
    insert_case(
        &db,
        tenant_id,
        future_employee,
        future_user,
        Uuid::new_v4(),
        "APPROVED",
        "2026-08-26".parse().unwrap(),
    )
    .await;

    let rejected_employee = Uuid::new_v4();
    let rejected_user = Uuid::new_v4();
    insert_case(
        &db,
        tenant_id,
        rejected_employee,
        rejected_user,
        Uuid::new_v4(),
        "REJECTED",
        today,
    )
    .await;

    assert_eq!(
        process_due_separations(&db, tenant_id, today)
            .await
            .unwrap()
            .processed,
        1
    );
    assert_eq!(
        process_due_separations(&db, tenant_id, today)
            .await
            .unwrap()
            .processed,
        0
    );

    let due = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            r#"SELECT e.status AS employee_status, u.is_active,
                      s.offboarded_at IS NOT NULL AS offboarded,
                      (SELECT COUNT(*) FROM user_session us WHERE us.user_id = u.id) AS sessions,
                      (SELECT COUNT(*) FROM outbox_event oe WHERE oe.aggregate_id = s.id
                         AND oe.event_type = 'employee.offboarded') AS events
               FROM separation s
               JOIN employee e ON e.id = s.employee_id
               JOIN "user" u ON u.id = e.user_id
               WHERE s.id = $1"#,
            vec![due_separation.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(due.try_get::<String>("", "employee_status").unwrap(), "INACTIVE");
    assert!(!due.try_get::<bool>("", "is_active").unwrap());
    assert!(due.try_get::<bool>("", "offboarded").unwrap());
    assert_eq!(due.try_get::<i64>("", "sessions").unwrap(), 0);
    assert_eq!(due.try_get::<i64>("", "events").unwrap(), 1);

    for employee_id in [future_employee, rejected_employee] {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT status FROM employee WHERE id = $1",
                vec![employee_id.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        assert_eq!(row.try_get::<String>("", "status").unwrap(), "ACTIVE");
    }
}
