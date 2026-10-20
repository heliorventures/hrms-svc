//! Run only against the separate synthetic fixture database prepared by
//! scripts/tests/fixtures/organization_tenant. Never loads deployment .env files.
use kabipay_organization_tenant::{bootstrap_once, statement};
use sea_orm::{ConnectOptions, ConnectionTrait, Database};
use uuid::Uuid;
#[tokio::test]
#[ignore = "requires the explicit synthetic organization_tenant_acceptance fixture"]
async fn source_bootstrap_resumes_in_bounded_batches_and_completes() {
    let url = std::env::var("KABIPAY_ORGANIZATION_SOURCE_TEST_DATABASE_URL")
        .expect("explicit synthetic URL is required");
    assert!(
        url.contains("127.0.0.1:15436/organization_tenant_acceptance"),
        "refusing a non-synthetic database"
    );
    let mut config = ConnectOptions::new(url);
    config.max_connections(1);
    let db = Database::connect(config).await.unwrap();
    db.execute(statement(
        "SET search_path=source_acceptance,public",
        vec![],
    ))
    .await
    .unwrap();
    let tenant = Uuid::parse_str("10000000-0000-0000-0000-000000000001").unwrap();
    let organization = Uuid::parse_str("20000000-0000-0000-0000-000000000001").unwrap();
    db.execute(statement("UPDATE organization_fact_bootstrap SET phase=0,after_id=NULL,status='PENDING' WHERE tenant_id=$1 AND organization_id=$2",vec![tenant.into(),organization.into()])).await.unwrap();
    let before: i64 = db
        .query_one(statement(
            "SELECT count(*) AS n FROM company_event_outbox",
            vec![],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap();
    assert!(bootstrap_once(&db, tenant, 0).await.is_err());
    assert!(bootstrap_once(&db, tenant, 201).await.is_err());
    // Limit one intentionally exercises non-terminal full pages and persisted cursors.
    for _ in 0..12 {
        if !bootstrap_once(&db, tenant, 1).await.unwrap() {
            break;
        }
    }
    let row=db.query_one(statement("SELECT phase,status FROM organization_fact_bootstrap WHERE tenant_id=$1 AND organization_id=$2",vec![tenant.into(),organization.into()])).await.unwrap().unwrap();
    assert_eq!(row.try_get::<i32>("", "phase").unwrap(), 5);
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "COMPLETE");
    assert!(!bootstrap_once(&db, tenant, 1).await.unwrap());
    let after: i64 = db
        .query_one(statement(
            "SELECT count(*) AS n FROM company_event_outbox",
            vec![],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "n")
        .unwrap();
    assert_eq!(after-before,2,"employee and leave snapshots only; physically deleted entry remains a previously emitted tombstone");
    let leaked:i64=db.query_one(statement("SELECT count(*) AS n FROM company_event_outbox WHERE event_envelope::text LIKE '%PRIVATE_%'",vec![])).await.unwrap().unwrap().try_get("","n").unwrap();
    assert_eq!(leaked, 0);
    for row in db
        .query_all(statement(
            "SELECT event_envelope FROM company_event_outbox",
            vec![],
        ))
        .await
        .unwrap()
    {
        let envelope: serde_json::Value = row.try_get("", "event_envelope").unwrap();
        let event: kabipay_organization_protocol::CompanyEvent =
            serde_json::from_value(envelope).unwrap();
        assert!(
            event.actor.is_some(),
            "new source events always retain explicit known or system actor evidence"
        );
    }
    let divergent:i64=db.query_one(statement("SELECT count(*) AS n FROM (SELECT aggregate_type,aggregate_id,aggregate_version FROM company_event_outbox GROUP BY aggregate_type,aggregate_id,aggregate_version HAVING count(DISTINCT (event_envelope-'event_id')::text)>1) conflicts",vec![])).await.unwrap().unwrap().try_get("","n").unwrap();
    assert_eq!(
        divergent, 0,
        "same-version snapshots must retain exact payload, actor and occurred_at"
    );
    assert_canonical_manager_effective_time(&db, tenant).await;
}

async fn assert_canonical_manager_effective_time(db: &sea_orm::DatabaseConnection, tenant: Uuid) {
    use kabipay_common::{
        canonical_reporting,
        client_data_scope::resolve_employee_scope_filter_with_connection,
        context::{ClientViewerEmployee, ScopeType},
    };
    let subject = Uuid::parse_str("30000000-0000-0000-0000-000000000001").unwrap();
    let old = Uuid::parse_str("a0000000-0000-0000-0000-000000000001").unwrap();
    let new = Uuid::parse_str("a0000000-0000-0000-0000-000000000002").unwrap();
    for (id, label) in [(old, "Previous manager"), (new, "New local manager")] {
        db.execute(statement("INSERT INTO employee(id,tenant_id,first_name,last_name,employee_code,status) VALUES($1,$2,$3,'Synthetic',$3,'ACTIVE') ON CONFLICT DO NOTHING",vec![id.into(),tenant.into(),label.into()])).await.unwrap();
    }
    let denied = db
        .execute(statement(
            "UPDATE employee SET reporting_manager_id=$3 WHERE tenant_id=$1 AND id=$2",
            vec![tenant.into(), subject.into(), old.into()],
        ))
        .await
        .unwrap_err();
    assert!(denied
        .to_string()
        .contains("Organization workspace manages reporting relationships"));
    db.execute(statement("UPDATE employee SET reporting_manager_id=reporting_manager_id WHERE tenant_id=$1 AND id=$2",vec![tenant.into(),subject.into()])).await.unwrap();
    db.execute(statement("INSERT INTO actor_ref(tenant_id,principal_id,display_name,identity_version,last_verified_at) VALUES($1,'b0000000-0000-0000-0000-000000000001','Subject',1,now()),($1,'b0000000-0000-0000-0000-000000000002','Manager',1,now()) ON CONFLICT DO NOTHING",vec![tenant.into()])).await.unwrap();
    db.execute(statement("INSERT INTO management_assignment(id,tenant_id,organization_id,subject_employee_id,subject_principal_id,manager_principal_id,subject_employment_ref_id,manager_employment_ref_id,manager_tenant_id,manager_employee_id,relationship_type,status,effective_from,effective_to,version) VALUES('c0000000-0000-0000-0000-000000000001',$1,'20000000-0000-0000-0000-000000000001',$2,'b0000000-0000-0000-0000-000000000001','b0000000-0000-0000-0000-000000000002','d0000000-0000-0000-0000-000000000001','d0000000-0000-0000-0000-000000000002','10000000-0000-0000-0000-000000000002',NULL,'PRIMARY','ACTIVE',now()-interval '1 day',now()+interval '1 day',1),('c0000000-0000-0000-0000-000000000002',$1,'20000000-0000-0000-0000-000000000001',$2,'b0000000-0000-0000-0000-000000000001','b0000000-0000-0000-0000-000000000002','d0000000-0000-0000-0000-000000000001','d0000000-0000-0000-0000-000000000003',$1,$3,'PRIMARY','ACTIVE',now()+interval '1 day',NULL,1)",vec![tenant.into(),subject.into(),new.into()])).await.unwrap();
    assert_eq!(
        canonical_reporting::manager(db, tenant, subject, Some(old))
            .await
            .unwrap(),
        None,
        "current foreign manager must prevent legacy local authority"
    );
    let scope = resolve_employee_scope_filter_with_connection(
        db,
        tenant,
        ScopeType::Team,
        Some(ClientViewerEmployee {
            employee_id: old,
            department_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(!scope.allows_employee(subject));
    let scope = resolve_employee_scope_filter_with_connection(
        db,
        tenant,
        ScopeType::Team,
        Some(ClientViewerEmployee {
            employee_id: new,
            department_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(
        !scope.allows_employee(subject),
        "future local manager must not acquire early authority"
    );
    db.execute(statement("UPDATE management_assignment SET effective_to=now()-interval '1 hour' WHERE id='c0000000-0000-0000-0000-000000000001'",vec![])).await.unwrap();
    db.execute(statement("UPDATE management_assignment SET effective_from=now()-interval '1 hour' WHERE id='c0000000-0000-0000-0000-000000000002'",vec![])).await.unwrap();
    assert_eq!(
        canonical_reporting::manager(db, tenant, subject, Some(old))
            .await
            .unwrap(),
        Some(new)
    );
    let scope = resolve_employee_scope_filter_with_connection(
        db,
        tenant,
        ScopeType::Team,
        Some(ClientViewerEmployee {
            employee_id: new,
            department_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(scope.allows_employee(subject));
    let scope = resolve_employee_scope_filter_with_connection(
        db,
        tenant,
        ScopeType::Team,
        Some(ClientViewerEmployee {
            employee_id: old,
            department_id: None,
        }),
    )
    .await
    .unwrap();
    assert!(!scope.allows_employee(subject));
}
