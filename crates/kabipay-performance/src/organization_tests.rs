//! Real PostgreSQL locking seam; deliberately minimal source tables, not a tenant upgrade.
use super::*;
use sea_orm::{ConnectOptions, Database, TransactionTrait};
use std::time::{Duration, Instant};

async fn connect(url: &str, schema: &str) -> DatabaseConnection {
    let mut options = ConnectOptions::new(url);
    options.max_connections(1);
    let db = Database::connect(options).await.unwrap();
    db.execute(statement(
        &format!("SET search_path=\"{schema}\",public"),
        vec![],
    ))
    .await
    .unwrap();
    db
}
async fn exercise(
    admin: &DatabaseConnection,
    local: &DatabaseConnection,
    source: &DatabaseConnection,
    tenant: Uuid,
    participant: Uuid,
    cycle: Uuid,
    replacement: Option<Uuid>,
) {
    let tx = local.begin().await.unwrap();
    performance_workflow::locked_cycle_stage(&tx, tenant, cycle)
        .await
        .unwrap();
    let command = OrganizationCommand {
        command_id: Uuid::new_v4(),
        resource_id: participant,
        expected_version: 1,
        operation: OrganizationOperation::ReviewSubmit,
        payload: serde_json::json!({}),
    };
    let db = source.clone();
    let (ready, waiting) = tokio::sync::oneshot::channel();
    let worker = tokio::spawn(async move {
        let source_tx = db.begin().await.unwrap();
        let pid: i32 = source_tx
            .query_one(statement("SELECT pg_backend_pid() AS pid", vec![]))
            .await
            .unwrap()
            .unwrap()
            .try_get("", "pid")
            .unwrap();
        ready.send(pid).unwrap();
        let result = PerformanceAdapter
            .prepare(&source_tx, tenant, &command)
            .await;
        source_tx.rollback().await.unwrap();
        result
    });
    let pid = waiting.await.unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        let row = admin
            .query_one(statement(
                "SELECT wait_event_type,query FROM pg_stat_activity WHERE pid=$1",
                vec![pid.into()],
            ))
            .await
            .unwrap()
            .unwrap();
        if row
            .try_get::<Option<String>>("", "wait_event_type")
            .unwrap()
            .as_deref()
            == Some("Lock")
        {
            assert!(
                row.try_get::<String>("", "query")
                    .unwrap()
                    .contains("review_cycle"),
                "source must first wait for the local cycle lock"
            );
            break;
        }
        assert!(
            Instant::now() < deadline,
            "source did not reach the cycle fence"
        );
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // If source locked participant first, this local cycle->participant path deadlocks.
    tokio::time::timeout(
        Duration::from_secs(2),
        tx.query_one(statement(
            "SELECT id FROM performance_participant WHERE tenant_id=$1 AND id=$2 FOR UPDATE",
            vec![tenant.into(), participant.into()],
        )),
    )
    .await
    .expect("source must not hold participant while blocked on cycle")
    .unwrap();
    if let Some(next) = replacement {
        tx.execute(statement(
            "UPDATE performance_participant SET review_cycle_id=$3 WHERE tenant_id=$1 AND id=$2",
            vec![tenant.into(), participant.into(), next.into()],
        ))
        .await
        .unwrap();
    }
    tx.commit().await.unwrap();
    let result = tokio::time::timeout(Duration::from_secs(5), worker)
        .await
        .unwrap()
        .unwrap();
    if replacement.is_some() {
        assert!(
            matches!(result, Err(KabiPayError::Conflict(_))),
            "source must reject a participant rebound across cycles"
        );
    } else {
        result.unwrap();
    }
}
#[tokio::test]
#[ignore = "requires explicit isolated synthetic PostgreSQL URL"]
async fn source_manager_prepare_uses_local_cycle_order_and_rejects_rebinding() {
    let url = std::env::var("KABIPAY_ORGANIZATION_SOURCE_TEST_DATABASE_URL")
        .expect("explicit synthetic URL required");
    assert!(
        url.contains("127.0.0.1:15436/organization_tenant_acceptance"),
        "refusing non-synthetic database"
    );
    let admin = Database::connect(&url).await.unwrap();
    let schema = format!("performance_order_{}", Uuid::new_v4().simple());
    admin.execute_unprepared(&format!("CREATE SCHEMA \"{schema}\"; CREATE TABLE \"{schema}\".review_cycle(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,current_stage TEXT NOT NULL); CREATE TABLE \"{schema}\".performance_participant(id UUID PRIMARY KEY,tenant_id UUID NOT NULL,review_cycle_id UUID NOT NULL,employee_id UUID NOT NULL,is_excluded BOOLEAN NOT NULL DEFAULT false)")).await.unwrap();
    let local = connect(&url, &schema).await;
    let source = connect(&url, &schema).await;
    let (tenant, participant, cycle, next, employee) = (
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
        Uuid::new_v4(),
    );
    local.execute(statement("INSERT INTO review_cycle(id,tenant_id,current_stage) VALUES($1,$3,'MANAGER_REVIEW'),($2,$3,'MANAGER_REVIEW')",vec![cycle.into(),next.into(),tenant.into()])).await.unwrap();
    local.execute(statement("INSERT INTO performance_participant(id,tenant_id,review_cycle_id,employee_id) VALUES($1,$2,$3,$4)",vec![participant.into(),tenant.into(),cycle.into(),employee.into()])).await.unwrap();
    exercise(&admin, &local, &source, tenant, participant, cycle, None).await;
    exercise(
        &admin,
        &local,
        &source,
        tenant,
        participant,
        cycle,
        Some(next),
    )
    .await;
    local.close().await.unwrap();
    source.close().await.unwrap();
    admin
        .execute(statement(
            &format!("DROP SCHEMA \"{schema}\" CASCADE"),
            vec![],
        ))
        .await
        .unwrap();
    admin.close().await.unwrap();
}

#[tokio::test]
#[ignore = "requires explicit isolated synthetic PostgreSQL URL"]
async fn expired_primary_without_review_caps_cannot_regain_authority_after_detach() {
    let url = std::env::var("KABIPAY_ORGANIZATION_SOURCE_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("127.0.0.1:15436/organization_tenant_acceptance"));
    let admin = Database::connect(&url).await.unwrap();
    let schema = format!("expired_primary_{}", Uuid::new_v4().simple());
    admin.execute_unprepared(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.employee(id UUID,tenant_id UUID,reporting_manager_id UUID); CREATE TABLE {schema}.management_assignment(id UUID,tenant_id UUID,subject_employee_id UUID,manager_employee_id UUID,relationship_type TEXT,status TEXT,effective_from TIMESTAMPTZ,effective_to TIMESTAMPTZ,graph_version BIGINT); CREATE TABLE {schema}.assignment_capability(tenant_id UUID,assignment_id UUID,capability_code TEXT); CREATE TABLE {schema}.organization_review_manager(tenant_id UUID,participant_id UUID); CREATE FUNCTION {schema}.organization_reporting_owned(UUID) RETURNS BOOLEAN LANGUAGE sql AS 'SELECT false'")).await.unwrap();
    let db = connect(&url, &schema).await;
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let old = Uuid::new_v4();
    let legacy = Uuid::new_v4();
    let assignment = Uuid::new_v4();
    let now = chrono::Utc::now();
    db.execute(statement(
        "INSERT INTO employee VALUES($1,$2,$3)",
        vec![employee.into(), tenant.into(), legacy.into()],
    ))
    .await
    .unwrap();
    db.execute(statement("INSERT INTO management_assignment VALUES($1,$2,$3,$4,'PRIMARY','ACTIVE',clock_timestamp()-interval '2 days',clock_timestamp()-interval '1 day',1)",vec![assignment.into(),tenant.into(),employee.into(),old.into()])).await.unwrap();
    let mut participant = performance_participant::Model {
        id: Uuid::new_v4(),
        tenant_id: tenant,
        review_cycle_id: Uuid::new_v4(),
        employee_id: employee,
        manager_employee_id: Some(old),
        department_id: None,
        designation_id: None,
        work_location_id: None,
        appraisal_template_id: Uuid::new_v4(),
        status: "MANAGER_REVIEW".into(),
        is_excluded: false,
        exclusion_reason: None,
        response_revision: 1,
        self_submitted_at: None,
        manager_submitted_at: None,
        acknowledged_at: None,
        acknowledgement_comment: None,
        final_rating: None,
        performance_band: None,
        manager_rating: None,
        manager_performance_band: None,
        calibration_provenance: None,
        created_at: now,
        updated_at: now,
    };
    // Terminal detached ownership is false; no review-capability marker was installed.
    for status in ["ACTIVE", "ENDED"] {
        db.execute(statement(
            "UPDATE management_assignment SET status=$1",
            vec![status.into()],
        ))
        .await
        .unwrap();
        participant.manager_employee_id = Some(old);
        crate::services::performance_management::apply_installed_manager(
            &db,
            tenant,
            &mut participant,
        )
        .await
        .unwrap();
        assert_eq!(
            participant.manager_employee_id,
            Some(legacy),
            "expired/retired canonical actor cannot regain local authority"
        );
    }
    db.close().await.unwrap();
    admin
        .execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
