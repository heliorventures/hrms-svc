use super::*;
use sea_orm::{ConnectOptions, Database, TransactionTrait};
#[tokio::test]
#[ignore = "requires explicit isolated synthetic PostgreSQL URL"]
async fn primary_end_restores_pending_fallback_without_rewriting_history_or_future() {
    let url = std::env::var("KABIPAY_ORGANIZATION_SOURCE_TEST_DATABASE_URL").unwrap();
    assert!(url.contains("127.0.0.1:15436/organization_tenant_acceptance"));
    let schema = format!("manager_end_{}", Uuid::new_v4().simple());
    let admin = Database::connect(&url).await.unwrap();
    admin.execute_unprepared(&format!("CREATE SCHEMA {schema}; CREATE TABLE {schema}.employee(id UUID,tenant_id UUID,reporting_manager_id UUID); CREATE TABLE {schema}.performance_participant(id UUID,tenant_id UUID,employee_id UUID,manager_employee_id UUID,manager_submitted_at TIMESTAMPTZ); CREATE TABLE {schema}.management_assignment(id UUID,tenant_id UUID,subject_employee_id UUID,manager_employee_id UUID,relationship_type TEXT,status TEXT,effective_from TIMESTAMPTZ,effective_to TIMESTAMPTZ)")).await.unwrap();
    let mut options = ConnectOptions::new(&url);
    options.max_connections(1);
    let db = Database::connect(options).await.unwrap();
    db.execute_unprepared(&format!("SET search_path={schema},public"))
        .await
        .unwrap();
    let tenant = Uuid::new_v4();
    let employee = Uuid::new_v4();
    let old = Uuid::new_v4();
    let legacy = Uuid::new_v4();
    let assignment = Uuid::new_v4();
    let pending = Uuid::new_v4();
    let submitted = Uuid::new_v4();
    db.execute(statement(
        "INSERT INTO employee VALUES($1,$2,$3)",
        vec![employee.into(), tenant.into(), legacy.into()],
    ))
    .await
    .unwrap();
    db.execute(statement("INSERT INTO performance_participant VALUES($1,$3,$4,$5,NULL),($2,$3,$4,$5,clock_timestamp())",vec![pending.into(),submitted.into(),tenant.into(),employee.into(),old.into()])).await.unwrap();
    db.execute(statement("INSERT INTO management_assignment VALUES($1,$2,$3,$4,'PRIMARY','ENDED',clock_timestamp()-interval '1 day',clock_timestamp())",vec![assignment.into(),tenant.into(),employee.into(),old.into()])).await.unwrap();
    let tx = db.begin().await.unwrap();
    rebind_pending_primary(&tx, tenant, employee, assignment, true)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let manager = |id:Uuid| {
        statement(
            "SELECT manager_employee_id FROM performance_participant WHERE id=$1",
            vec![id.into()],
        )
    };
    assert_eq!(
        db.query_one(manager(pending))
            .await
            .unwrap()
            .unwrap()
            .try_get::<Uuid>("", "manager_employee_id")
            .unwrap(),
        legacy
    );
    assert_eq!(
        db.query_one(manager(submitted))
            .await
            .unwrap()
            .unwrap()
            .try_get::<Uuid>("", "manager_employee_id")
            .unwrap(),
        old
    );
    db.execute(statement("UPDATE management_assignment SET status='ACTIVE',effective_from=clock_timestamp()+interval '1 day',effective_to=NULL",vec![])).await.unwrap();
    let tx = db.begin().await.unwrap();
    rebind_pending_primary(&tx, tenant, employee, assignment, false)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        db.query_one(manager(pending))
            .await
            .unwrap()
            .unwrap()
            .try_get::<Uuid>("", "manager_employee_id")
            .unwrap(),
        legacy
    );
    db.execute(statement("UPDATE management_assignment SET effective_from=clock_timestamp()-interval '1 day',manager_employee_id=NULL",vec![])).await.unwrap();
    let tx = db.begin().await.unwrap();
    rebind_pending_primary(&tx, tenant, employee, assignment, false)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(
        db.query_one(manager(pending))
            .await
            .unwrap()
            .unwrap()
            .try_get::<Option<Uuid>>("", "manager_employee_id")
            .unwrap(),
        None
    );
    db.close().await.unwrap();
    admin
        .execute_unprepared(&format!("DROP SCHEMA {schema} CASCADE"))
        .await
        .unwrap();
}
