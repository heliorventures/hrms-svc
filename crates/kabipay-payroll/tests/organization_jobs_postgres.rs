//! Disposable PostgreSQL evidence for durable job fencing and commit recovery.
use kabipay_organization_tenant::statement;
use kabipay_payroll::organization_job_worker::claim;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, TransactionTrait};
use uuid::Uuid;

#[tokio::test]
#[ignore = "requires KABIPAY_ORGANIZATION_JOB_TEST_DATABASE_URL for a disposable PostgreSQL fixture"]
async fn durable_acceptance_restart_fence_and_business_commit_are_atomic() {
    let url = std::env::var("KABIPAY_ORGANIZATION_JOB_TEST_DATABASE_URL")
        .expect("disposable fixture URL");
    assert!(
        url.starts_with("postgres://")
            && url.contains("@127.0.0.1:")
            && url
                .split('?')
                .next()
                .is_some_and(|s| s.ends_with("_fixture")),
        "job test requires an explicit loopback disposable fixture database"
    );
    let schema = format!("job_fixture_{}", Uuid::new_v4().simple());
    let control = Database::connect(url.clone()).await.unwrap();
    let mut options = ConnectOptions::new(url);
    options
        .max_connections(1)
        .min_connections(1)
        .set_schema_search_path(format!("{schema},public"))
        .sqlx_logging(false);
    let db = Database::connect(options).await.unwrap();
    db.execute_unprepared(&format!("CREATE SCHEMA {schema}; SET search_path TO {schema},public; CREATE TABLE payroll_cycle(id uuid PRIMARY KEY,tenant_id uuid NOT NULL); CREATE TABLE organization_resource_version(tenant_id uuid NOT NULL,resource_type text NOT NULL,resource_id uuid NOT NULL,version bigint NOT NULL,PRIMARY KEY(tenant_id,resource_type,resource_id)); CREATE TABLE fixture_business(id uuid PRIMARY KEY);" )).await.unwrap();
    let migration=include_str!("../../../../hrms-database/changelog/migrations/0119_organization_payroll_jobs/organization_payroll_jobs.xml");
    let sql = migration
        .split("<![CDATA[")
        .nth(1)
        .unwrap()
        .split("]]>")
        .next()
        .unwrap()
        .replace("${schema}", &schema);
    db.execute_unprepared(&sql).await.unwrap();
    let tenant = Uuid::new_v4();
    let other = Uuid::new_v4();
    let cycle = Uuid::new_v4();
    let job = Uuid::new_v4();
    db.execute(statement(
        "INSERT INTO payroll_cycle(id,tenant_id) VALUES($1,$2)",
        vec![cycle.into(), tenant.into()],
    ))
    .await
    .unwrap();
    let insert="INSERT INTO organization_payroll_job(tenant_id,id,cycle_id,principal_id,organization_id,membership_id,authority_id,authority_version,operation,request_body,request_hash,authority_reference,expected_version) VALUES($1,$2,$3,$4,$5,$6,$7,1,'payroll_draft',$8,$9,'{}',1)";
    let values = vec![
        tenant.into(),
        job.into(),
        cycle.into(),
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        Uuid::new_v4().into(),
        b"{}".to_vec().into(),
        "a".repeat(64).into(),
    ];
    // A crash before acceptance commit leaves neither a claimable job nor work.
    let tx = db.begin().await.unwrap();
    tx.execute(statement(insert, values.clone())).await.unwrap();
    tx.rollback().await.unwrap();
    assert!(claim(&db, tenant).await.unwrap().is_none());
    db.execute(statement(insert, values.clone())).await.unwrap();
    let mut conflicting = values.clone();
    conflicting[1] = Uuid::new_v4().into();
    assert!(
        db.execute(statement(insert, conflicting)).await.is_err(),
        "a distinct command cannot create another active job for the same cycle"
    );
    assert!(
        db.execute(statement(insert, values)).await.is_err(),
        "original command creates only one job"
    );
    assert!(
        claim(&db, other).await.unwrap().is_none(),
        "wrong company cannot claim job"
    );
    assert!(
        db.query_one(statement(
            "SELECT id FROM organization_payroll_job WHERE tenant_id=$1 AND id=$2",
            vec![other.into(), job.into()],
        ))
        .await
        .unwrap()
        .is_none(),
        "job reads remain company bound"
    );
    assert!(
        db.execute(statement(
            "UPDATE organization_payroll_job SET request_body=$3 WHERE tenant_id=$1 AND id=$2",
            vec![tenant.into(), job.into(), b"substituted".to_vec().into()],
        ))
        .await
        .is_err(),
        "durable original intent cannot be substituted"
    );
    let (claimed, first) = claim(&db, tenant).await.unwrap().unwrap();
    assert_eq!(claimed, job);
    assert!(
        claim(&db, tenant).await.unwrap().is_none(),
        "live lease excludes a second worker"
    );
    // Restart recovers only after the durable lease expires.
    db.execute(statement("UPDATE organization_payroll_job SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND id=$2",vec![tenant.into(),job.into()])).await.unwrap();
    let (_, second) = claim(&db, tenant).await.unwrap().unwrap();
    assert_ne!(first, second);
    assert_eq!(db.execute(statement("UPDATE organization_payroll_job SET state='SUCCEEDED',finished_at=clock_timestamp(),lease_token=NULL,lease_expires_at=NULL WHERE tenant_id=$1 AND id=$2 AND lease_token=$3",vec![tenant.into(),job.into(),first.into()])).await.unwrap().rows_affected(),0,"stale worker is fenced");
    // The database also checks the original lease at COMMIT, closing the gap
    // between the worker's final UPDATE and a delayed commit request.
    db.execute(statement("UPDATE organization_payroll_job SET lease_expires_at=clock_timestamp()+interval '200 milliseconds' WHERE tenant_id=$1 AND id=$2",vec![tenant.into(),job.into()])).await.unwrap();
    let tx = db.begin().await.unwrap();
    tx.execute(statement(
        "INSERT INTO fixture_business(id) VALUES($1)",
        vec![job.into()],
    ))
    .await
    .unwrap();
    tx.execute(statement("UPDATE organization_payroll_job SET state='SUCCEEDED',finished_at=clock_timestamp(),lease_token=NULL,lease_expires_at=NULL WHERE tenant_id=$1 AND id=$2 AND lease_token=$3",vec![tenant.into(),job.into(),second.into()])).await.unwrap();
    tx.execute_unprepared("SELECT pg_sleep(0.3)").await.unwrap();
    assert!(
        tx.commit().await.is_err(),
        "expired lease at commit must roll back business and terminal job state"
    );
    assert!(db
        .query_one(statement(
            "SELECT id FROM fixture_business WHERE id=$1",
            vec![job.into()]
        ))
        .await
        .unwrap()
        .is_none());
    let (_, second) = claim(&db, tenant).await.unwrap().unwrap();
    // Terminate only this fixture transaction's backend before business commit.
    // PostgreSQL must recover both business and job rows after connection loss.
    let tx = db.begin().await.unwrap();
    let backend = tx
        .query_one(statement("SELECT pg_backend_pid() AS pid", vec![]))
        .await
        .unwrap()
        .unwrap()
        .try_get::<i32>("", "pid")
        .unwrap();
    tx.execute(statement(
        "INSERT INTO fixture_business(id) VALUES($1)",
        vec![job.into()],
    ))
    .await
    .unwrap();
    tx.execute(statement("UPDATE organization_payroll_job SET state='SUCCEEDED',finished_at=clock_timestamp(),lease_token=NULL,lease_expires_at=NULL WHERE tenant_id=$1 AND id=$2 AND lease_token=$3",vec![tenant.into(),job.into(),second.into()])).await.unwrap();
    let killed = control
        .query_one(statement(
            "SELECT pg_terminate_backend($1) AS killed",
            vec![backend.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<bool>("", "killed")
        .unwrap();
    assert!(killed);
    let _ = tx.rollback().await;
    assert!(db
        .query_one(statement(
            "SELECT id FROM fixture_business WHERE id=$1",
            vec![job.into()]
        ))
        .await
        .unwrap()
        .is_none());
    assert_eq!(
        db.query_one(statement(
            "SELECT state FROM organization_payroll_job WHERE tenant_id=$1 AND id=$2",
            vec![tenant.into(), job.into()]
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get::<String>("", "state")
        .unwrap(),
        "RUNNING"
    );
    db.execute(statement("UPDATE organization_payroll_job SET lease_expires_at=clock_timestamp()-interval '1 second' WHERE tenant_id=$1 AND id=$2",vec![tenant.into(),job.into()])).await.unwrap();
    let (_, recovered) = claim(&db, tenant).await.unwrap().unwrap();
    assert_ne!(recovered, second);
    let tx = db.begin().await.unwrap();
    tx.execute(statement(
        "INSERT INTO fixture_business(id) VALUES($1)",
        vec![job.into()],
    ))
    .await
    .unwrap();
    tx.execute(statement("UPDATE organization_payroll_job SET state='SUCCEEDED',finished_at=clock_timestamp(),lease_token=NULL,lease_expires_at=NULL WHERE tenant_id=$1 AND id=$2 AND lease_token=$3",vec![tenant.into(),job.into(),recovered.into()])).await.unwrap();
    tx.commit().await.unwrap();
    assert!(
        claim(&db, tenant).await.unwrap().is_none(),
        "restart after commit cannot rerun terminal job"
    );
    assert!(db.execute(statement("UPDATE organization_payroll_job SET state='QUEUED',finished_at=NULL WHERE tenant_id=$1 AND id=$2",vec![tenant.into(),job.into()])).await.is_err(),"terminal outcome cannot be rewritten");
    // This drops only the unpredictable fixture schema created by this test.
    db.execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
