use super::*;
use sea_orm::{DbBackend, QueryTrait};

#[test]
fn queue_filters_before_pagination_beyond_120_and_counts_full_scope() {
    let mut queue = QueuePage::new(2, 1);
    // The first matching request is older than the former 120-row cap.
    for id in 0..130 {
        queue.consider(id, "APPROVED", false, Some("PENDING"), true);
    }
    queue.consider(130, "PENDING", false, Some("PENDING"), true);
    for id in 131..135 {
        queue.consider(id, "PENDING", true, Some("PENDING"), true);
    }
    assert_eq!(queue.rows, vec![132, 133]);
    assert_eq!(queue.total_count, 4);
    assert_eq!(queue.pending_count, 5);
    assert_eq!(queue.actionable_count, 4);
}

#[test]
fn queue_counts_are_independent_of_status_and_out_of_range_pages() {
    let mut queue = QueuePage::new(20, 99);
    queue.consider(1, "PENDING", false, Some("APPROVED"), false);
    queue.consider(2, "PENDING", true, Some("APPROVED"), false);
    queue.consider(3, "APPROVED", false, Some("APPROVED"), false);
    assert!(queue.rows.is_empty());
    assert_eq!((queue.total_count, queue.pending_count, queue.actionable_count), (1, 2, 1));
}

#[test]
fn queue_sql_binds_tenant_deletion_overlap_scope_and_stable_cursor() {
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let cursor_id = Uuid::new_v4();
    let date = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
    let filter = EmployeeScopeFilter::EmployeeIds(vec![employee]);
    let sql = candidate_query(tenant, &filter, Some(date), Some(date), Some((date.and_hms_opt(0, 0, 0).unwrap().and_utc(), cursor_id)))
        .build(DbBackend::Postgres).to_string();
    for expected in [tenant.to_string(), employee.to_string(), cursor_id.to_string(), "is_deleted".into(), "to_date".into(), "from_date".into(), "ORDER BY".into(), "LIMIT 1000".into()] {
        assert!(sql.contains(&expected), "missing {expected}: {sql}");
    }
    let empty_sql = candidate_query(tenant, &EmployeeScopeFilter::Empty, None, None, None)
        .build(DbBackend::Postgres).to_string();
    assert!(empty_sql.contains("1 = 2"), "empty scope must fail closed: {empty_sql}");
    assert!(
        sql.contains("(\"leave_request\".\"applied_at\", \"leave_request\".\"id\") <"),
        "cursor must be a row comparison usable as an index condition: {sql}"
    );
}

#[test]
fn queue_rejects_invalid_filters_before_database_access() {
    assert!(validate_filters(0, None, None, None).is_err());
    assert!(validate_filters(201, None, None, None).is_err());
    assert!(validate_filters(1000, None, None, None).is_err());
    assert!(validate_filters(20, None, None, Some("UNKNOWN")).is_err());
    let from = NaiveDate::from_ymd_opt(2026, 9, 10).unwrap();
    let to = NaiveDate::from_ymd_opt(2026, 9, 1).unwrap();
    assert!(validate_filters(20, Some(from), Some(to), None).is_err());
    assert!(validate_filters(200, None, None, Some("PENDING")).is_ok());
}
#[tokio::test]
#[ignore = "requires explicitly provisioned disposable PostgreSQL on loopback port 15439"]
async fn postgres_queue_authority_parity_scaling_and_snapshot() {
    use sea_orm::{ConnectOptions, Database, DbErr, Schema, Statement};
    use kabipay_common::context::ClientViewerEmployee;
    use kabipay_db_entities::tenant::d0025_workflow::workflow_action;
    let url = std::env::var("KABIPAY_QUEUE_TEST_DATABASE_URL").expect("explicit disposable database URL required");
    assert_eq!(url, "postgresql://queue_test:queue_test_local_only@127.0.0.1:15439/leave_queue_test", "test refuses any other database");
    let schema_name = format!("queue_test_{}", Uuid::new_v4().simple());
    let mut options = ConnectOptions::new(url.clone());
    options.max_connections(1);
    let db = Database::connect(options).await.unwrap();
    db.execute_unprepared(&format!("CREATE SCHEMA {schema_name}; SET search_path TO {schema_name}, public")).await.unwrap();
    let schema = Schema::new(DbBackend::Postgres);
    for statement in [
        schema.create_table_from_entity(employee::Entity), schema.create_table_from_entity(leave_request::Entity),
        schema.create_table_from_entity(workflow::Entity), schema.create_table_from_entity(workflow_step::Entity),
        schema.create_table_from_entity(workflow_instance::Entity), schema.create_table_from_entity(workflow_action::Entity),
    ] { db.execute(DbBackend::Postgres.build(&statement)).await.unwrap(); }
    db.execute_unprepared("CREATE EXTENSION IF NOT EXISTS pg_stat_statements WITH SCHEMA public").await.unwrap();
    let tenant = Uuid::new_v4();
    let actor = Uuid::new_v4();
    let subject = Uuid::new_v4();
    let actor_user = Uuid::new_v4();
    let wf = Uuid::new_v4();
    let step = Uuid::new_v4();
    db.execute_unprepared(&format!(r#"
        INSERT INTO employee (id,tenant_id,user_id,reporting_manager_id,employee_code,first_name,last_name,status,date_of_joining,is_deleted,created_at,updated_at)
        VALUES ('{actor}','{tenant}','{actor_user}',NULL,'actor','Actor','User','ACTIVE',CURRENT_DATE,FALSE,NOW(),NOW()),
               ('{subject}','{tenant}',NULL,'{actor}','subject','Subject','Employee','ACTIVE',CURRENT_DATE,FALSE,NOW(),NOW());
        INSERT INTO workflow VALUES ('{wf}','{tenant}','Leave','LEAVE_REQUEST',TRUE,NOW(),NOW());
        INSERT INTO workflow_step (id,tenant_id,workflow_id,sequence_order,step_name,approver_type,approver_permission,can_skip,created_at,updated_at)
        VALUES ('{step}','{tenant}','{wf}',1,'Approval','MANAGER','leave:approve',FALSE,NOW(),NOW());
        INSERT INTO leave_request (id,tenant_id,employee_id,leave_type_id,from_date,to_date,days_requested,is_half_day,status,workflow_instance_id,uses_comp_off,applied_at,is_deleted,created_at,updated_at)
        SELECT md5(g::text)::uuid,'{tenant}','{subject}','{wf}',CURRENT_DATE,CURRENT_DATE,1,FALSE,'PENDING',md5(g::text)::uuid,FALSE,NOW(),FALSE,NOW(),NOW() FROM generate_series(1,200) g;
        INSERT INTO workflow_instance (id,tenant_id,workflow_id,entity_type,entity_id,status,current_step_id,created_at,updated_at)
        SELECT id,'{tenant}','{wf}','LEAVE_REQUEST',id,'IN_PROGRESS','{step}',NOW(),NOW() FROM leave_request;
    "#)).await.unwrap();
    let authority = WorkflowApprovalAuthority { actor_user_id: actor_user, actor_employee: Some(ClientViewerEmployee { employee_id: actor, department_id: None }), scope: ScopeType::All, permission: PERM_LEAVE_APPROVE };
    let unrestricted = WorkflowApprovalScope::from(&EmployeeScopeFilter::Unrestricted);
    let empty = WorkflowApprovalScope::from(&EmployeeScopeFilter::Empty);
    let rows = candidate_query(tenant, &EmployeeScopeFilter::Unrestricted, None, None, None).all(&db).await.unwrap();
    assert_eq!(rows.len(), 200);
    // Compare the existing single-record authority execution against the new bulk path.
    for (kind, permission, expected) in [
        ("MANAGER", "leave:approve", true), ("PERMISSION", " LEAVE:APPROVE ", true),
        ("PERMISSION", "expense:approve", false), ("ROLE", "leave:approve", false),
        ("MANAGER_OR_ROLE", "leave:approve", false), ("MANAGER_OR_PERMISSION", "expense:approve", true),
        ("UNKNOWN", "leave:approve", false),
    ] {
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "UPDATE workflow_step SET approver_type=$1, approver_permission=$2", [kind.into(), permission.into()])).await.unwrap();
        let bulk = batch_actionable_steps(&db, tenant, &rows[..1], Some(&authority), &unrestricted).await.unwrap();
        let row = &rows[0];
        let single = crate::services::leave_service::resolve_actionable_leave_workflow_step_id(&db, tenant, row.id, &row.status, row.employee_id, row.workflow_instance_id, &authority).await.unwrap();
        assert_eq!(bulk.get(&row.id).copied(), single, "parity for {kind}");
        assert_eq!(single.is_some(), expected, "eligibility for {kind}");
    }
    db.execute_unprepared("UPDATE workflow_step SET approver_type='PERMISSION', approver_permission='leave:approve'").await.unwrap();
    for (change, restore) in [
        ("UPDATE employee SET is_deleted=TRUE WHERE employee_code='subject'", "UPDATE employee SET is_deleted=FALSE"),
        ("UPDATE employee SET status='INACTIVE' WHERE employee_code='subject'", "UPDATE employee SET status='ACTIVE'"),
        ("UPDATE employee SET status='INACTIVE' WHERE employee_code='actor'", "UPDATE employee SET status='ACTIVE'"),
        ("UPDATE workflow_instance SET current_step_id=NULL", ""),
    ] {
        db.execute_unprepared(change).await.unwrap();
        let row = &rows[0];
        let bulk = batch_actionable_steps(&db, tenant, &rows[..1], Some(&authority), &unrestricted).await.unwrap();
        let single = crate::services::leave_service::resolve_actionable_leave_workflow_step_id(&db, tenant, row.id, &row.status, row.employee_id, row.workflow_instance_id, &authority).await.unwrap();
        assert_eq!(bulk.get(&row.id).copied(), single);
        assert!(single.is_none());
        if !restore.is_empty() { db.execute_unprepared(restore).await.unwrap(); }
    }
    // Missing current pointers retain logical display stage but remain non-actionable.
    let titles = kabipay_common::workflow_current_step::pending_step_titles_batch(&db, tenant, &[rows[0].id]).await.unwrap();
    assert_eq!(titles.get(&rows[0].id).map(String::as_str), Some("Approval"));
    let single_title = kabipay_common::workflow_inbox::pending_workflow_step_title(&db, tenant, "PENDING", "PENDING", Some(rows[0].id)).await.unwrap();
    assert_eq!(titles.get(&rows[0].id).cloned(), single_title);
    db.execute_unprepared(&format!("INSERT INTO workflow_action (id,tenant_id,instance_id,workflow_step_id,action,acted_at,created_at,updated_at) VALUES ('{}','{tenant}','{}','{step}','APPROVE',NOW(),NOW(),NOW())", Uuid::new_v4(), rows[0].id)).await.unwrap();
    assert!(kabipay_common::workflow_current_step::pending_step_titles_batch(&db, tenant, &[rows[0].id]).await.unwrap().is_empty());
    assert!(kabipay_common::workflow_inbox::pending_workflow_step_title(&db, tenant, "PENDING", "PENDING", Some(rows[0].id)).await.unwrap().is_none());
    db.execute_unprepared("DELETE FROM workflow_action").await.unwrap();
    db.execute_unprepared(&format!("UPDATE workflow_instance SET current_step_id='{step}'")).await.unwrap();
    assert!(batch_actionable_steps(&db, tenant, &rows, Some(&authority), &empty).await.unwrap().is_empty());
    assert!(batch_actionable_steps(&db, Uuid::new_v4(), &rows, Some(&authority), &unrestricted).await.unwrap().is_empty());
    let mut self_row = rows[0].clone();
    self_row.employee_id = actor;
    let mut legacy_row = rows[0].clone();
    legacy_row.workflow_instance_id = None;
    let mut historical_row = rows[0].clone();
    historical_row.status = "APPROVED".into();
    for row in [self_row, legacy_row, historical_row] {
        assert!(batch_actionable_steps(&db, tenant, std::slice::from_ref(&row), Some(&authority), &unrestricted).await.unwrap().is_empty());
        assert!(crate::services::leave_service::resolve_actionable_leave_workflow_step_id(&db, tenant, row.id, &row.status, row.employee_id, row.workflow_instance_id, &authority).await.unwrap().is_none());
    }
    for change in [
        format!("UPDATE workflow_instance SET entity_id='{actor}'"),
        "UPDATE workflow_instance SET entity_type='EXPENSE'".to_owned(),
        "UPDATE workflow_instance SET status='COMPLETED'".to_owned(),
        format!("UPDATE workflow_step SET workflow_id='{actor}'"),
        format!("UPDATE workflow_step SET tenant_id='{actor}'"),
        "UPDATE workflow SET entity_type='EXPENSE'".to_owned(),
    ] {
        db.execute_unprepared(&change).await.unwrap();
        let row = &rows[0];
        assert!(batch_actionable_steps(&db, tenant, &rows[..1], Some(&authority), &unrestricted).await.unwrap().is_empty());
        assert!(crate::services::leave_service::resolve_actionable_leave_workflow_step_id(&db, tenant, row.id, &row.status, row.employee_id, row.workflow_instance_id, &authority).await.unwrap().is_none());
        db.execute_unprepared(&format!("UPDATE workflow_instance SET entity_id=id, entity_type='LEAVE_REQUEST', status='IN_PROGRESS'; UPDATE workflow_step SET workflow_id='{wf}', tenant_id='{tenant}'; UPDATE workflow SET entity_type='LEAVE_REQUEST'")).await.unwrap();
    }
    let team_authority = WorkflowApprovalAuthority { scope: ScopeType::Team, ..authority.clone() };
    let team_filter = resolve_employee_scope_filter_with_connection(&db, tenant, ScopeType::Team, authority.actor_employee).await.unwrap();
    assert!(team_filter.allows_employee(subject));
    assert_eq!(batch_actionable_steps(&db, tenant, &rows[..1], Some(&team_authority), &WorkflowApprovalScope::from(&team_filter)).await.unwrap().get(&rows[0].id).copied(),
        crate::services::leave_service::resolve_actionable_leave_workflow_step_id(&db, tenant, rows[0].id, &rows[0].status, rows[0].employee_id, rows[0].workflow_instance_id, &team_authority).await.unwrap());
    // pg_stat_statements counts actual server statements, not an implementation mock.
    async fn select_calls(db: &sea_orm::DatabaseConnection) -> Result<i64, DbErr> {
        let result = db.query_one(Statement::from_string(DbBackend::Postgres,
            "SELECT COALESCE(SUM(calls),0)::bigint AS calls FROM public.pg_stat_statements WHERE query ~ '^[[:space:]]*SELECT' AND query NOT LIKE '%pg_stat_statements%'".to_owned())).await?.unwrap();
        result.try_get("", "calls")
    }
    let mut counts = Vec::new();
    for length in [1, 200] {
        let before = select_calls(&db).await.unwrap();
        assert_eq!(batch_actionable_steps(&db, tenant, &rows[..length], Some(&authority), &unrestricted).await.unwrap().len(), length);
        counts.push(select_calls(&db).await.unwrap() - before);
    }
    assert_eq!(counts, [5, 5], "authority query count must be independent of batch length");
    let stage_ids: Vec<_> = rows.iter().map(|row| row.id).collect();
    for length in [1, 200] {
        let before = select_calls(&db).await.unwrap();
        assert_eq!(kabipay_common::workflow_current_step::pending_step_titles_batch(&db, tenant, &stage_ids[..length]).await.unwrap().len(), length);
        assert_eq!(select_calls(&db).await.unwrap() - before, 1, "stage enrichment uses one SELECT regardless of page length");
    }
    // Execute the complete production loader through GraphQL, including nested cache fields.
    // Only tenant pool resolution is injected; scope, transaction, paging and enrichment are real.
    struct QueueQuery;
    #[async_graphql::Object]
    impl QueueQuery {
        async fn queue(&self, ctx: &Context<'_>, status: Option<String>, needs_my_action: bool, offset: Option<u64>) -> Result<LeaveApprovalQueue> {
            let db = ctx.data::<sea_orm::DatabaseConnection>()?.clone();
            let claims = ctx.data::<kabipay_common::context::ClientClaims>()?;
            load_from_db(ctx, db, claims.tenant_id, ScopeType::All, 20, offset.unwrap_or(0), None, None, status.as_deref(), needs_my_action).await
        }
    }
    let claims = kabipay_common::context::ClientClaims {
        sub: actor_user, iss: kabipay_common::context::CLIENT_JWT_ISSUER.into(), exp: 0, iat: 0,
        tenant_id: tenant, email: String::new(), employee_id: Some(actor), must_change_password: false,
        roles: vec![], permissions: vec![PERM_LEAVE_APPROVE.into()],
        permission_scopes: HashMap::from([(PERM_LEAVE_APPROVE.into(), "ALL".into())]), resource_scopes: HashMap::new(),
    };
    let graphql = async_graphql::Schema::build(QueueQuery, async_graphql::EmptyMutation, async_graphql::EmptySubscription)
        .data(db.clone()).data(claims).finish();
    let query = "{ queue(needsMyAction: true) { totalCount pendingCount actionableCount rows { id employeeName pendingApprovalStage viewerMayApprove pendingApprovalStepId } } }";
    for (length, expected_queries) in [(1, 9), (200, 9)] {
        db.execute_unprepared(&format!("UPDATE leave_request SET is_deleted = id <> '{}'", rows[0].id)).await.unwrap();
        if length == 200 { db.execute_unprepared("UPDATE leave_request SET is_deleted=FALSE").await.unwrap(); }
        let before = select_calls(&db).await.unwrap();
        let response = graphql.execute(query).await;
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["queue"]["totalCount"], length);
        assert_eq!(data["queue"]["pendingCount"], length);
        assert_eq!(data["queue"]["actionableCount"], length);
        assert_eq!(data["queue"]["rows"].as_array().unwrap().len(), std::cmp::min(length, 20) as usize);
        assert_eq!(data["queue"]["rows"][0]["employeeName"], "Subject Employee");
        assert_eq!(data["queue"]["rows"][0]["pendingApprovalStage"], "Approval");
        assert_eq!(data["queue"]["rows"][0]["viewerMayApprove"], true);
        assert_eq!(data["queue"]["rows"][0]["pendingApprovalStepId"], step.to_string());
        assert_eq!(select_calls(&db).await.unwrap() - before, expected_queries, "complete loader and nested fields for {length} candidates");
    }
    db.execute_unprepared("UPDATE leave_request SET status='APPROVED' WHERE id IN (SELECT id FROM leave_request ORDER BY id LIMIT 25)").await.unwrap();
    let response = graphql.execute("{ queue(status: \"APPROVED\", needsMyAction: false) { totalCount pendingCount actionableCount rows { status viewerMayApprove pendingApprovalStepId pendingApprovalStage } } }").await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.into_json().unwrap();
    assert_eq!(data["queue"]["totalCount"], 25);
    assert_eq!(data["queue"]["pendingCount"], 175);
    assert_eq!(data["queue"]["actionableCount"], 175);
    assert_eq!(data["queue"]["rows"][0]["viewerMayApprove"], false);
    assert!(data["queue"]["rows"][0]["pendingApprovalStepId"].is_null());
    assert!(data["queue"]["rows"][0]["pendingApprovalStage"].is_null());
    db.execute_unprepared("UPDATE leave_request SET status='PENDING'").await.unwrap();
    db.execute_unprepared(&format!(r#"
        INSERT INTO leave_request (id,tenant_id,employee_id,leave_type_id,from_date,to_date,days_requested,is_half_day,status,workflow_instance_id,uses_comp_off,applied_at,is_deleted,created_at,updated_at)
        SELECT md5(g::text)::uuid,'{tenant}','{subject}','{wf}',CURRENT_DATE,CURRENT_DATE,1,FALSE,'PENDING',md5(g::text)::uuid,FALSE,NOW(),FALSE,NOW(),NOW() FROM generate_series(201,2001) g;
        INSERT INTO workflow_instance (id,tenant_id,workflow_id,entity_type,entity_id,status,current_step_id,created_at,updated_at)
        SELECT id,'{tenant}','{wf}','LEAVE_REQUEST',id,'IN_PROGRESS','{step}',NOW(),NOW()
        FROM leave_request r WHERE NOT EXISTS (SELECT 1 FROM workflow_instance i WHERE i.id=r.id);
    "#)).await.unwrap();
    let before = select_calls(&db).await.unwrap();
    let response = graphql.execute(query).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.into_json().unwrap();
    assert_eq!(data["queue"]["totalCount"], 2001);
    assert_eq!(data["queue"]["pendingCount"], 2001);
    assert_eq!(data["queue"]["actionableCount"], 2001);
    assert_eq!(select_calls(&db).await.unwrap() - before, 21, "three candidate batches each use one scan and five authority queries, plus viewer and page enrichment");
    let thousand_rows = candidate_query(tenant, &EmployeeScopeFilter::Unrestricted, None, None, None)
        .all(&db).await.unwrap();
    assert_eq!(thousand_rows.len(), 1000, "internal candidate memory bound");
    let before = select_calls(&db).await.unwrap();
    assert_eq!(batch_actionable_steps(&db, tenant, &thousand_rows, Some(&authority), &unrestricted)
        .await.unwrap().len(), 1000);
    assert_eq!(select_calls(&db).await.unwrap() - before, 5, "authority batch bound does not introduce record-level queries");
    // Apply the real production index migration only inside this disposable schema.
    let migration_path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../../hrms-database/changelog/migrations/0081_leave_approval_queue_index/leave_approval_queue_index.xml");
    let migration = std::fs::read_to_string(migration_path).expect("production 0081 migration required");
    let index_sql = migration.split("<![CDATA[").nth(1).unwrap().split("]]>").next().unwrap()
        .replace("${schema}", &schema_name);
    db.execute_unprepared(&index_sql).await.unwrap();
    db.execute_unprepared(&format!(r#"
        INSERT INTO leave_request (id,tenant_id,employee_id,leave_type_id,from_date,to_date,days_requested,is_half_day,status,workflow_instance_id,uses_comp_off,applied_at,is_deleted,created_at,updated_at)
        SELECT md5(g::text)::uuid,'{tenant}','{subject}','{wf}',CURRENT_DATE,CURRENT_DATE,1,FALSE,'PENDING',md5(g::text)::uuid,FALSE,NOW(),FALSE,NOW(),NOW() FROM generate_series(2002,10000) g;
        INSERT INTO workflow_instance (id,tenant_id,workflow_id,entity_type,entity_id,status,current_step_id,created_at,updated_at)
        SELECT id,'{tenant}','{wf}','LEAVE_REQUEST',id,'IN_PROGRESS','{step}',NOW(),NOW()
        FROM leave_request r WHERE NOT EXISTS (SELECT 1 FROM workflow_instance i WHERE i.id=r.id);
        ANALYZE leave_request;
        ANALYZE workflow_instance;
    "#)).await.unwrap();
    for offset in [0, 9800] {
        let expected = candidate_query(tenant, &EmployeeScopeFilter::Unrestricted, None, None, None)
            .offset(offset).limit(20).all(&db).await.unwrap();
        let benchmark_query = format!("{{ queue(needsMyAction: true, offset: {offset}) {{ totalCount pendingCount actionableCount rows {{ id employeeName pendingApprovalStage viewerMayApprove pendingApprovalStepId }} }} }}");
        let before = select_calls(&db).await.unwrap();
        let started = std::time::Instant::now();
        let response = graphql.execute(benchmark_query).await;
        let elapsed = started.elapsed();
        assert!(response.errors.is_empty(), "{:?}", response.errors);
        let data = response.data.into_json().unwrap();
        assert_eq!(data["queue"]["totalCount"], 10000);
        assert_eq!(data["queue"]["pendingCount"], 10000);
        assert_eq!(data["queue"]["actionableCount"], 10000);
        let actual_ids: Vec<_> = data["queue"]["rows"].as_array().unwrap().iter()
            .map(|row| row["id"].as_str().unwrap().to_owned()).collect();
        assert_eq!(actual_ids, expected.iter().map(|row| row.id.to_string()).collect::<Vec<_>>());
        let calls = select_calls(&db).await.unwrap() - before;
        assert_eq!(calls, 64, "10 full batches, terminal probe, viewer and page enrichment");
        eprintln!("Queue full-loader benchmark: candidates=10000 offset={offset} elapsed_ms={} SELECTs={calls}", elapsed.as_millis());
    }
    // Concurrent commit cannot change candidates, authority or labels inside the queue snapshot.
    let snapshot = db.begin_with_config(Some(IsolationLevel::RepeatableRead), Some(AccessMode::ReadOnly)).await.unwrap();
    let snapshot_team = resolve_employee_scope_filter_with_connection(&snapshot, tenant, ScopeType::Team, authority.actor_employee).await.unwrap();
    assert!(snapshot_team.allows_employee(subject));
    let before = batch_actionable_steps(&snapshot, tenant, &rows, Some(&authority), &unrestricted).await.unwrap();
    let mut writer_options = ConnectOptions::new(url);
    writer_options.max_connections(1);
    let writer = Database::connect(writer_options).await.unwrap();
    writer.execute_unprepared(&format!("SET search_path TO {schema_name}, public; UPDATE workflow_step SET approver_permission='expense:approve'; UPDATE leave_request SET status='APPROVED'; UPDATE employee SET first_name='Changed', reporting_manager_id=NULL WHERE employee_code='subject'")).await.unwrap();
    let after = batch_actionable_steps(&snapshot, tenant, &rows, Some(&authority), &unrestricted).await.unwrap();
    assert_eq!(before, after);
    assert!(candidate_query(tenant, &EmployeeScopeFilter::Unrestricted, None, None, None).all(&snapshot).await.unwrap().iter().all(|row| row.status == "PENDING"));
    assert_eq!(employee::Entity::find_by_id(subject).one(&snapshot).await.unwrap().unwrap().first_name, "Subject");
    assert!(resolve_employee_scope_filter_with_connection(&snapshot, tenant, ScopeType::Team, authority.actor_employee).await.unwrap().allows_employee(subject));
    snapshot.commit().await.unwrap();
    assert!(batch_actionable_steps(&db, tenant, &rows, Some(&authority), &unrestricted).await.unwrap().is_empty());
    assert!(!resolve_employee_scope_filter_with_connection(&db, tenant, ScopeType::Team, authority.actor_employee).await.unwrap().allows_employee(subject));
    writer.close().await.unwrap();
    db.execute_unprepared(&format!("DROP SCHEMA {schema_name} CASCADE")).await.unwrap();
    eprintln!("PostgreSQL: 1 and 200 pending rows each use 5 authority SELECTs; live parity and concurrent snapshot checks passed");
}
