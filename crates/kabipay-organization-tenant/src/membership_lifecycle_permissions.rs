//! Actual installer exercised only by the owned native PostgreSQL fixture driver.
use super::{install, statement};
use kabipay_common::KabiPayError;
use kabipay_organization_protocol::MembershipInstallation;
use kabipay_organization_security::command_body_hash;
use sea_orm::{ConnectOptions, ConnectionTrait, Database, DatabaseConnection, TransactionTrait};
use std::time::{Duration, Instant};
use uuid::Uuid;

fn seal(mut value: MembershipInstallation) -> MembershipInstallation {
    value.configuration_hash.clear();
    value.configuration_hash = command_body_hash(&serde_json::to_vec(&value).expect("canonical configuration"));
    value
}
async fn connection(url: &str) -> DatabaseConnection {
    let mut options = ConnectOptions::new(url.to_owned());
    options.max_connections(1).min_connections(1).sqlx_logging(false).set_schema_search_path("source,public");
    Database::connect(options).await.expect("owned loopback fixture")
}
async fn wait_for_lock(observer: &DatabaseConnection, pid: i32) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while Instant::now() < deadline {
        let row = observer.query_one(statement("SELECT wait_event_type FROM pg_stat_activity WHERE pid=$1",vec![pid.into()])).await.expect("observe fixture lock").expect("waiting backend");
        if row.try_get::<Option<String>>("", "wait_event_type").expect("wait type").as_deref() == Some("Lock") {
            return;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("second installer must block on the retained company advisory transaction lock");
}

#[tokio::test]
#[ignore = "requires the owned PostgreSQL16 membership-receipt fixture driver"]
async fn immutable_receipt_serializes_same_command_across_memberships() {
    let url = std::env::var("LIFECYCLE_PERMISSION_TEST_DATABASE_URL").expect("explicit owned fixture URL");
    let parsed = reqwest::Url::parse(&url).expect("fixture URL");
    assert_eq!(parsed.scheme(), "postgres");
    assert_eq!(parsed.host_str(), Some("127.0.0.1"));
    assert_eq!(parsed.username(), "lifecycle_runtime");
    assert_eq!(parsed.path(), "/lifecycle_fixture");
    assert!(parsed.password().is_none() && parsed.query().is_none());
    assert!(parsed.port().is_some_and(|port| port > 1024));
    let first = connection(&url).await;
    let second = connection(&url).await;
    let observer = connection(&url).await;
    let first_pid = first.query_one(statement("SELECT pg_backend_pid() AS pid",vec![])).await.expect("pid").expect("row").try_get::<i32>("","pid").expect("pid");
    let second_pid = second.query_one(statement("SELECT pg_backend_pid() AS pid",vec![])).await.expect("pid").expect("row").try_get::<i32>("","pid").expect("pid");
    let permission = observer.query_one(statement("SELECT has_any_column_privilege(current_user,'source.organization_membership_lifecycle_receipt','UPDATE') AS allowed",vec![])).await.expect("ACL").expect("row");
    assert!(!permission.try_get::<bool>("","allowed").expect("permission"));
    let value = seal(MembershipInstallation {
        organization_id: Uuid::new_v4(),membership_id: Uuid::new_v4(),tenant_id: Uuid::new_v4(),
        status: "ACTIVE".into(),version: 1,configuration_hash: String::new(),
    });
    let command = Uuid::new_v4();
    let tx = first.begin().await.expect("first transaction");
    let accepted = install(&tx,&value,command).await.expect("installer needs no immutable receipt UPDATE privilege");
    let locks = observer.query_one(statement("SELECT count(*) AS n FROM pg_locks WHERE pid=$1 AND locktype='advisory' AND granted",vec![first_pid.into()])).await.expect("advisory locks").expect("row").try_get::<i64>("","n").expect("count");
    assert_eq!(locks,2,"both company and tenant-command advisory locks must survive through commit");
    let other = seal(MembershipInstallation {organization_id:Uuid::new_v4(),membership_id:Uuid::new_v4(),..value.clone()});
    let attempt = tokio::spawn(async move {
        let other_tx = second.begin().await.expect("second transaction");
        let outcome = install(&other_tx,&other,command).await;
        other_tx.rollback().await.expect("discard mismatched command");
        outcome
    });
    wait_for_lock(&observer,second_pid).await;
    tx.commit().await.expect("first installation commit");
    assert!(matches!(attempt.await.expect("second installer"),Err(KabiPayError::Conflict(_))),"same tenant command cannot create a different membership after the first commit");
    let replay = first.begin().await.expect("exact replay");
    let repeated = install(&replay,&value,command).await.expect("same immutable command is idempotent");
    assert_eq!(repeated.configuration_hash,accepted.configuration_hash);
    replay.commit().await.expect("replay commit");
    let mismatch = first.begin().await.expect("mismatch transaction");
    let changed = seal(MembershipInstallation {version:2,..value.clone()});
    assert!(matches!(install(&mismatch,&changed,command).await,Err(KabiPayError::Conflict(_))),"receipt hash/revision mismatch must fail before installation mutation");
    mismatch.rollback().await.expect("mismatch rollback");
    let receipt = observer.query_one(statement("SELECT membership_id,organization_id,version,configuration_hash,(SELECT count(*) FROM organization_membership_installation) AS installations,(SELECT count(*) FROM organization_membership_lifecycle_receipt) AS receipts FROM organization_membership_lifecycle_receipt WHERE tenant_id=$1 AND command_id=$2",vec![value.tenant_id.into(),command.into()])).await.expect("immutable final state").expect("receipt");
    assert_eq!(receipt.try_get::<Uuid>("","membership_id").expect("membership"),value.membership_id);
    assert_eq!(receipt.try_get::<Uuid>("","organization_id").expect("organization"),value.organization_id);
    assert_eq!(receipt.try_get::<i64>("","version").expect("revision"),1);
    assert_eq!(receipt.try_get::<String>("","configuration_hash").expect("hash"),value.configuration_hash);
    assert_eq!(receipt.try_get::<i64>("","installations").expect("installations"),1);
    assert_eq!(receipt.try_get::<i64>("","receipts").expect("receipts"),1);
    let installed = observer.query_one(statement("SELECT membership_id,organization_id,version,configuration_hash,status,desired_status,retirement_transaction_id FROM organization_membership_installation WHERE tenant_id=$1",vec![value.tenant_id.into()])).await.expect("unchanged installation").expect("installation");
    assert_eq!(installed.try_get::<Uuid>("","membership_id").expect("installed membership"),value.membership_id);
    assert_eq!(installed.try_get::<Uuid>("","organization_id").expect("installed organization"),value.organization_id);
    assert_eq!(installed.try_get::<i64>("","version").expect("installed revision"),value.version);
    assert_eq!(installed.try_get::<String>("","configuration_hash").expect("installed hash"),value.configuration_hash);
    assert_eq!(installed.try_get::<String>("","status").expect("installed status"),"ACTIVE");
    assert_eq!(installed.try_get::<String>("","desired_status").expect("installed desired status"),"ACTIVE");
    assert!(installed.try_get::<Option<i64>>("","retirement_transaction_id").expect("retirement fence").is_none());
}
