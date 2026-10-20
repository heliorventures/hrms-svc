use chrono::{NaiveDate, Utc};
use kabipay_common::due_offboarding::process_due_separations;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DbBackend, Statement};
use uuid::Uuid;

async fn test_db() -> Option<sea_orm::DatabaseConnection> {
    let url = std::env::var("PRIVATE_LEGACY_TEST_DATABASE_URL").ok()?;
    assert!(
        url.starts_with("postgresql://private_fixture:private_fixture@127.0.0.1:")
            && url.ends_with("/private_retirement_fixture"),
        "refusing a non-fixture database"
    );
    let mut options = ConnectOptions::new(url);
    options.max_connections(1).min_connections(1);
    Some(
        Database::connect(options)
            .await
            .expect("connect disposable PostgreSQL database"),
    )
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
CREATE TEMP TABLE private_source_cutover(tenant_id uuid PRIMARY KEY,producer_generation bigint NOT NULL);
CREATE TEMP TABLE private_domain_receipt(id uuid PRIMARY KEY,tenant_id uuid,event_type text,aggregate_type text,aggregate_id uuid,occurred_at timestamptz,source_hash char(64));
CREATE TEMP TABLE webhook_subscription(id uuid PRIMARY KEY,tenant_id uuid,event_name text,is_active boolean,config_version bigint,created_at timestamptz);
CREATE TEMP TABLE private_delivery_receipt(tenant_id uuid,intent_id uuid,destination_id uuid,source_kind text,resource_id uuid,intent_hash char(64),destination_version bigint,status text,source_generation bigint,created_at timestamptz,updated_at timestamptz,outcome_code text,PRIMARY KEY(tenant_id,intent_id,destination_id));
CREATE TEMP TABLE company_event_outbox(id uuid PRIMARY KEY,source_tenant_id uuid,organization_id uuid,event_type text,aggregate_type text,aggregate_id uuid,aggregate_version bigint,event_envelope jsonb,event_stream text,created_at timestamptz);
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
#[ignore = "requires explicit disposable PRIVATE_LEGACY_TEST_DATABASE_URL"]
async fn due_offboarding_is_tenant_scoped_idempotent_and_status_aware() {
    let Some(db) = test_db().await else {
        panic!("explicit isolated source fixture URL required");
    };
    create_fixture_tables(&db).await;
    let tenant_id = Uuid::new_v4();
    let keys=serde_json::to_vec(&serde_json::json!({"schema_version":1,"keys":[{"tenant_id":tenant_id,"purpose":"delivery","key_id":"fixture","key_b64":"BwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwcHBwc=","active":true}]})).unwrap();
    let keyfile = std::env::temp_dir().join(format!("offboarding-key-{}.json", Uuid::new_v4()));
    std::fs::write(&keyfile, &keys).unwrap();
    let old_key = std::env::var_os("KABIPAY_PRIVATE_EVENT_KEYS_FILE");
    let old_skip = std::env::var_os("OUTBOX_SKIP_TENANT_WEBHOOKS");
    let old_platform = std::env::var_os("OUTBOX_WEBHOOK_URL");
    std::env::set_var("KABIPAY_PRIVATE_EVENT_KEYS_FILE", &keyfile);
    std::env::remove_var("OUTBOX_SKIP_TENANT_WEBHOOKS");
    std::env::remove_var("OUTBOX_WEBHOOK_URL");
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "INSERT INTO private_source_cutover VALUES($1,1)",
        [tenant_id.into()],
    ))
    .await
    .unwrap();
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
                      (SELECT COUNT(*) FROM private_domain_receipt oe WHERE oe.aggregate_id = s.id
                         AND oe.event_type = 'employee.offboarded' AND oe.id=s.offboarding_event_id AND oe.occurred_at=s.offboarded_at) AS events,
                      (SELECT COUNT(*) FROM private_delivery_receipt d WHERE d.resource_id=s.offboarding_event_id AND d.source_kind='webhook') AS intents
               FROM separation s
               JOIN employee e ON e.id = s.employee_id
               JOIN "user" u ON u.id = e.user_id
               WHERE s.id = $1"#,
            vec![due_separation.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        due.try_get::<String>("", "employee_status").unwrap(),
        "INACTIVE"
    );
    assert!(!due.try_get::<bool>("", "is_active").unwrap());
    assert!(due.try_get::<bool>("", "offboarded").unwrap());
    assert_eq!(due.try_get::<i64>("", "sessions").unwrap(), 0);
    assert_eq!(due.try_get::<i64>("", "events").unwrap(), 1);
    assert_eq!(due.try_get::<i64>("", "intents").unwrap(), 1);
    let rows=db.query_all(Statement::from_string(DbBackend::Postgres,"SELECT event_envelope FROM company_event_outbox WHERE event_stream='delivery' ORDER BY event_envelope->>'frame_index'".to_string())).await.unwrap();
    let frames: Vec<kabipay_common::private_events::crypto::Envelope> = rows
        .into_iter()
        .map(|r| {
            serde_json::from_value(
                r.try_get::<serde_json::Value>("", "event_envelope")
                    .unwrap(),
            )
            .unwrap()
        })
        .collect();
    let bytes = kabipay_common::private_events::crypto::Keyring::from_json(&keys)
        .unwrap()
        .decrypt_frames(&frames, tenant_id, "delivery")
        .unwrap();
    let intent: kabipay_common::private_events::DeliveryIntentInput =
        serde_json::from_slice(&bytes).unwrap();
    assert_eq!(intent.event_type, "employee.offboarded");
    assert_eq!(
        intent.payload["payload"]["employee_id"],
        serde_json::json!(due_employee)
    );

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
    for (name, old) in [
        ("KABIPAY_PRIVATE_EVENT_KEYS_FILE", old_key),
        ("OUTBOX_SKIP_TENANT_WEBHOOKS", old_skip),
        ("OUTBOX_WEBHOOK_URL", old_platform),
    ] {
        match old {
            Some(v) => std::env::set_var(name, v),
            None => std::env::remove_var(name),
        }
    }
    std::fs::remove_file(keyfile).unwrap();
    db.close().await.unwrap();
}
