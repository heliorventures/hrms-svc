//! Only the owned Node loopback fixture can launch this ignored rehearsal.
use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use kabipay_organization_security::{
    ServiceCredentialIssuer, ServiceCredentialVerifier, ServiceVerificationKey,
};
use kabipay_organization_tenant::{
    email_identity::resolve_email_recipient,
    email_transport::{consume_email_proof, verify_recipient_transport, VerifiedEmailRecipient},
};
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait,
};
use serde::Deserialize;
use std::{path::PathBuf, time::Duration as Timeout};
use uuid::Uuid;
fn id(n: u32) -> Uuid {
    Uuid::parse_str(&format!("00000000-0000-4000-8000-{n:012}")).unwrap()
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OwnedFixture {
    port: u16,
    system_id: String,
    data_directory: PathBuf,
}
fn fixture() -> OwnedFixture {
    let path = PathBuf::from(
        std::env::var_os("KABIPAY_EMAIL_RECIPIENT_OWNED_FIXTURE")
            .expect("use the owned Node fixture"),
    );
    assert!(path.is_absolute() && std::fs::metadata(&path).unwrap().len() < 16384);
    let fixture: OwnedFixture = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    let data = fixture.data_directory.canonicalize().unwrap();
    assert_eq!(
        data.parent().unwrap(),
        path.parent().unwrap().canonicalize().unwrap()
    );
    assert!(data
        .parent()
        .unwrap()
        .file_name()
        .unwrap()
        .to_string_lossy()
        .starts_with("hrms-shared-grants-"));
    let pid = std::fs::read_to_string(data.join("postmaster.pid")).unwrap();
    let lines: Vec<_> = pid.lines().collect();
    assert_eq!(PathBuf::from(lines[1]).canonicalize().unwrap(), data);
    assert_eq!(lines[3].parse::<u16>().unwrap(), fixture.port);
    assert!(fixture.port > 1024 && fixture.port != 5432 && fixture.port != 5433);
    fixture
}
async fn connect(fixture: &OwnedFixture, user: &str) -> DatabaseConnection {
    assert!(matches!(
        user,
        "fixture_owner" | "hrms_company_employee" | "hrms_fixture_metadata_employee"
    ));
    let mut options=ConnectOptions::new(format!("postgres://{user}:synthetic-unused@127.0.0.1:{}/company_email_recipient_fixture?sslmode=disable",fixture.port));
    options
        .max_connections(2)
        .min_connections(1)
        .connect_timeout(Timeout::from_secs(5))
        .sqlx_logging(false)
        .set_schema_search_path("tenant_fixture,pg_catalog");
    Database::connect(options).await.unwrap()
}
async fn execute(owner: &DatabaseConnection, sql: &str) {
    owner
        .execute(Statement::from_string(DbBackend::Postgres, sql.to_owned()))
        .await
        .unwrap();
}

fn request(relationship: bool, local: bool, test: bool) -> EmailEligibilityRequest {
    let now = Utc::now();
    let tenant = if local || test { id(2) } else { id(1) };
    let resource = if test {
        EmailResourceType::Test
    } else {
        EmailResourceType::Leave
    };
    let recipient = if local || test {
        EmailRecipientRef::LocalUser { user_id: id(26) }
    } else {
        EmailRecipientRef::OrganizationPrincipal {
            principal_id: id(23),
            source_tenant_id: id(2),
            source_employee_id: id(25),
        }
    };
    let authorization_ref = if test {
        EmailAuthorizationRef::DomainTransition {
            resource_type: resource,
            transition_key: "settings-test".into(),
        }
    } else if local {
        EmailAuthorizationRef::LocalWorkflow {
            instance_id: id(41),
            step_id: id(42),
        }
    } else if relationship {
        EmailAuthorizationRef::OrganizationRelationship {
            organization_id: id(20),
            membership_id: id(21),
            relationship_id: id(30),
            version: 1,
        }
    } else {
        EmailAuthorizationRef::OrganizationGrant {
            organization_id: id(20),
            membership_id: id(21),
            binding_id: id(27),
            version: 1,
        }
    };
    let event = EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: tenant,
        resource_type: resource,
        resource_id: id(40),
        resource_version: "pending:1".into(),
        category: if test {
            EmailCategory::Test
        } else {
            EmailCategory::Leave
        },
        kind: if test {
            EmailEventKind::Test
        } else {
            EmailEventKind::ActionRequired
        },
        template_version: 1,
        recipient,
        actor_ref: None,
        authorization_ref,
        action_target: if local || test {
            EmailActionTarget::CompanyResource {
                resource_type: resource,
                resource_id: id(40),
            }
        } else {
            EmailActionTarget::OrganizationResource {
                organization_id: id(20),
                membership_id: id(21),
                resource_type: resource,
                resource_id: id(40),
            }
        },
        occurred_at: now,
        expires_at: now + Duration::hours(1),
    };
    EmailEligibilityRequest {
        intent_id: Uuid::new_v4(),
        event_hash: event.canonical_hash().unwrap(),
        event,
    }
}
fn proof(request: &EmailEligibilityRequest) -> VerifiedEmailRecipient {
    let service = format!("kabipay-{}", request.event.producer());
    let tenant = request.event.tenant_id;
    let issuer = ServiceCredentialIssuer::new(
        &service,
        "mail-fixture",
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-private.pem"),
    )
    .unwrap();
    let verifier = ServiceCredentialVerifier::new(
        EMAIL_RECIPIENT_AUDIENCE,
        vec![ServiceVerificationKey {
            kid: "mail-fixture".into(),
            service,
            tenant_ids: vec![tenant],
            public_pem: include_bytes!(
                "../../kabipay-organization-security/tests/fixtures/test-a-public.pem"
            )
            .to_vec(),
        }],
    )
    .unwrap();
    let body = serde_json::to_vec(request).unwrap();
    let token = issuer
        .issue(
            EMAIL_RECIPIENT_AUDIENCE,
            tenant,
            EMAIL_RECIPIENT_ROUTE,
            &body,
        )
        .unwrap();
    verify_recipient_transport(&verifier, &token, request, &body).unwrap()
}
async fn recipient(
    source: &DatabaseConnection,
    metadata: &DatabaseConnection,
    request: &EmailEligibilityRequest,
) -> EmailRecipientResolution {
    resolve_email_recipient(
        source,
        metadata,
        &proof(request),
        "https://source.example.invalid",
    )
    .await
    .unwrap()
}

#[tokio::test]
#[ignore = "run only through the owned local PostgreSQL fixture"]
async fn owned_recipient_authority_and_identity() {
    let fixture = fixture();
    let owner = connect(&fixture, "fixture_owner").await;
    let system: String = owner
        .query_one(Statement::from_string(
            DbBackend::Postgres,
            "SELECT system_identifier::text AS id FROM pg_control_system()".to_owned(),
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "id")
        .unwrap();
    assert_eq!(system, fixture.system_id);
    let source = connect(&fixture, "hrms_company_employee").await;
    let metadata = connect(&fixture, "hrms_fixture_metadata_employee").await;
    let request = request(false, false, false);
    let verified = proof(&request);
    let EmailRecipientResolution::Recipient { recipient: cross } = resolve_email_recipient(
        &source,
        &metadata,
        &verified,
        "https://source.example.invalid",
    )
    .await
    .unwrap() else {
        panic!("active verified employment must resolve")
    };
    assert_eq!(
        cross.identity,
        EmailRecipientIdentity {
            source_tenant_id: id(2),
            source_user_id: id(26)
        }
    );
    assert_eq!(cross.address(), "recipient@example.invalid");
    assert!(!format!("{cross:?}").contains("recipient@"));
    assert!(!serde_json::to_string(&request)
        .unwrap()
        .contains("recipient@"));
    let tx = source.begin().await.unwrap();
    consume_email_proof(&tx, verified.credential(), id(2))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let tx = source.begin().await.unwrap();
    assert!(consume_email_proof(&tx, verified.credential(), id(2))
        .await
        .is_err());
    tx.rollback().await.unwrap();
    execute(&owner, &format!("INSERT INTO tenant_fixture.email_private_request_receipt(tenant_id,purpose,audience,nonce,request_hash,created_at,expires_at) SELECT '{}','email_recipient','kabipay-email-recipient',gen_random_uuid(),repeat('a',64),clock_timestamp()-interval '60 seconds',clock_timestamp()-interval '1 second' FROM generate_series(1,105)", id(2))).await;
    let tx = source.begin().await.unwrap();
    consume_email_proof(&tx, proof(&request).credential(), id(2))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let expired:i64=owner.query_one(Statement::from_string(DbBackend::Postgres,"SELECT count(*)::bigint AS n FROM tenant_fixture.email_private_request_receipt WHERE expires_at<clock_timestamp()".to_owned())).await.unwrap().unwrap().try_get("","n").unwrap();
    assert_eq!(
        expired, 5,
        "one authenticated lookup purges at most 100 expired receipts"
    );
    let tx = source.begin().await.unwrap();
    assert!(
        consume_email_proof(&tx, verified.credential(), id(2))
            .await
            .is_err(),
        "unexpired proofs remain protected after cleanup"
    );
    tx.rollback().await.unwrap();
    let EmailRecipientResolution::Recipient { recipient: local } =
        recipient(&source, &metadata, &self::request(false, true, false)).await
    else {
        panic!("local identity must resolve")
    };
    assert_eq!(
        local.identity, cross.identity,
        "local/organization aliases yield one stable dispatch identity"
    );
    // Account-authorized Profile/Performance reviewers need not have employment.
    // The exception is only local ActionRequired after a fresh owning-domain proof.
    execute(&owner, "UPDATE tenant_fixture.employee SET is_deleted=true").await;
    for resource in [
        EmailResourceType::ProfileChange,
        EmailResourceType::PerformanceReview,
    ] {
        let mut account = self::request(false, true, false);
        account.event.resource_type = resource;
        account.event.category = EmailCategory::Approval;
        account.event.authorization_ref = EmailAuthorizationRef::DomainTransition {
            resource_type: resource,
            transition_key: "pending:1".into(),
        };
        account.event.action_target = EmailActionTarget::CompanyResource {
            resource_type: resource,
            resource_id: account.event.resource_id,
        };
        account.event_hash = account.event.canonical_hash().unwrap();
        assert!(matches!(
            recipient(&source, &metadata, &account).await,
            EmailRecipientResolution::Recipient { .. }
        ));
        account.event.kind = EmailEventKind::Outcome;
        account.event_hash = account.event.canonical_hash().unwrap();
        assert!(
            resolve_email_recipient(
                &source,
                &metadata,
                &proof(&account),
                "https://source.example.invalid"
            )
            .await
            .is_err(),
            "an outcome still requires active employment"
        );
    }
    assert!(
        resolve_email_recipient(
            &source,
            &metadata,
            &proof(&self::request(false, true, false)),
            "https://source.example.invalid"
        )
        .await
        .is_err(),
        "Leave does not inherit the account-only exception"
    );
    assert!(
        resolve_email_recipient(
            &source,
            &metadata,
            &proof(&request),
            "https://source.example.invalid"
        )
        .await
        .is_err(),
        "organization routing never inherits the account-only exception"
    );
    execute(
        &owner,
        "UPDATE tenant_fixture.employee SET is_deleted=false",
    )
    .await;
    for(change,restore)in[
  ("UPDATE kabipay_ops.principal_role_binding SET status='REVOKED'","UPDATE kabipay_ops.principal_role_binding SET status='ACTIVE'"),
  ("UPDATE kabipay_ops.principal_role_binding SET version=2","UPDATE kabipay_ops.principal_role_binding SET version=1"),
  ("UPDATE kabipay_ops.principal_login_link SET status='REVOKED'","UPDATE kabipay_ops.principal_login_link SET status='VERIFIED'"),
  ("UPDATE tenant_fixture.\"user\" SET is_active=false WHERE id='00000000-0000-4000-8000-000000000026'","UPDATE tenant_fixture.\"user\" SET is_active=true WHERE id='00000000-0000-4000-8000-000000000026'"),
  ("UPDATE tenant_fixture.employee SET user_id=NULL WHERE id='00000000-0000-4000-8000-000000000025'","UPDATE tenant_fixture.employee SET user_id='00000000-0000-4000-8000-000000000026' WHERE id='00000000-0000-4000-8000-000000000025'"),
 ]{
  execute(&owner,change).await;assert!(resolve_email_recipient(&source,&metadata,&proof(&request),"https://source.example.invalid").await.is_err());execute(&owner,restore).await;
 }
    let relationship = self::request(true, false, false);
    assert!(matches!(
        recipient(&source, &metadata, &relationship).await,
        EmailRecipientResolution::Recipient { .. }
    ));
    execute(
        &owner,
        "UPDATE kabipay_ops.organization_relationship SET status='ENDED'",
    )
    .await;
    assert!(resolve_email_recipient(
        &source,
        &metadata,
        &proof(&relationship),
        "https://source.example.invalid"
    )
    .await
    .is_err());
    execute(&owner,"UPDATE tenant_fixture.\"user\" SET email=NULL WHERE id='00000000-0000-4000-8000-000000000026'").await;
    assert!(matches!(
        recipient(&source, &metadata, &request).await,
        EmailRecipientResolution::Unavailable
    ));
    let test = self::request(false, true, true);
    assert!(
        matches!(
            recipient(&source, &metadata, &test).await,
            EmailRecipientResolution::TestAuthorized { .. }
        ),
        "settings test authority returns no mailbox even when the actor has none"
    );
    execute(&owner,"DELETE FROM tenant_fixture.role_permission WHERE permission_id='00000000-0000-4000-8000-000000000053'").await;
    assert!(
        resolve_email_recipient(
            &source,
            &metadata,
            &proof(&test),
            "https://source.example.invalid"
        )
        .await
        .is_err(),
        "test permission is rechecked after queue creation"
    );
}
