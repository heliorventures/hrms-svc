use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use kabipay_organization_security::{
    ServiceCredentialIssuer, ServiceCredentialVerifier, ServiceVerificationKey,
};
use kabipay_organization_tenant::{email_identity::resolve_email_recipient, email_transport::*};
use sea_orm::{
    entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr,
    ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement,
};
use std::{
    collections::{BTreeMap, VecDeque},
    sync::{Arc, Mutex},
};
use uuid::Uuid;

#[derive(Debug)]
struct Replies {
    rows: Mutex<VecDeque<Vec<ProxyRow>>>,
    queries: Arc<Mutex<Vec<String>>>,
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Replies {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        self.queries.lock().unwrap().push(statement.to_string());
        self.rows
            .lock()
            .unwrap()
            .pop_front()
            .ok_or(DbErr::Custom("unexpected address or metadata query".into()))
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom("unexpected write".into()))
    }
}
async fn db(rows: Vec<Vec<ProxyRow>>) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
    let queries = Arc::new(Mutex::new(vec![]));
    let db = Database::connect_proxy(
        DbBackend::Postgres,
        Arc::new(Box::new(Replies {
            rows: Mutex::new(rows.into()),
            queries: queries.clone(),
        })),
    )
    .await
    .unwrap();
    (db, queries)
}
fn proof(action: bool, relationship: bool) -> VerifiedEmailRecipient {
    let now = Utc::now();
    let resource = Uuid::new_v4();
    let tenant = Uuid::new_v4();
    let org = Uuid::new_v4();
    let membership = Uuid::new_v4();
    let event = EmailEvent {
        event_id: Uuid::new_v4(),
        tenant_id: tenant,
        resource_type: EmailResourceType::Leave,
        resource_id: resource,
        resource_version: "decision:1".into(),
        category: EmailCategory::Leave,
        kind: if action {
            EmailEventKind::ActionRequired
        } else {
            EmailEventKind::Outcome
        },
        template_version: 1,
        recipient: EmailRecipientRef::OrganizationPrincipal {
            principal_id: Uuid::new_v4(),
            source_tenant_id: Uuid::new_v4(),
            source_employee_id: Uuid::new_v4(),
        },
        actor_ref: None,
        authorization_ref: if action && relationship {
            EmailAuthorizationRef::OrganizationRelationship {
                organization_id: org,
                membership_id: membership,
                relationship_id: Uuid::new_v4(),
                version: 1,
            }
        } else if action {
            EmailAuthorizationRef::OrganizationGrant {
                organization_id: org,
                membership_id: membership,
                binding_id: Uuid::new_v4(),
                version: 1,
            }
        } else {
            EmailAuthorizationRef::DomainTransition {
                resource_type: EmailResourceType::Leave,
                transition_key: "approved:1".into(),
            }
        },
        action_target: EmailActionTarget::OrganizationResource {
            organization_id: org,
            membership_id: membership,
            resource_type: EmailResourceType::Leave,
            resource_id: resource,
        },
        occurred_at: now,
        expires_at: now + Duration::hours(1),
    };
    let request = EmailEligibilityRequest {
        intent_id: Uuid::new_v4(),
        event_hash: event.canonical_hash().unwrap(),
        event,
    };
    let body = serde_json::to_vec(&request).unwrap();
    let issuer = ServiceCredentialIssuer::new(
        "kabipay-leave",
        "mail-test",
        include_bytes!("../../kabipay-organization-security/tests/fixtures/test-a-private.pem"),
    )
    .unwrap();
    let verifier = ServiceCredentialVerifier::new(
        EMAIL_RECIPIENT_AUDIENCE,
        vec![ServiceVerificationKey {
            kid: "mail-test".into(),
            service: "kabipay-leave".into(),
            tenant_ids: vec![tenant],
            public_pem: include_bytes!(
                "../../kabipay-organization-security/tests/fixtures/test-a-public.pem"
            )
            .to_vec(),
        }],
    )
    .unwrap();
    let token = issuer
        .issue(
            EMAIL_RECIPIENT_AUDIENCE,
            tenant,
            EMAIL_RECIPIENT_ROUTE,
            &body,
        )
        .unwrap();
    verify_recipient_transport(&verifier, &token, &request, &body).unwrap()
}

#[tokio::test]
async fn forged_source_employment_denied_before_address_read() {
    let (source, source_reads) = db(vec![]).await;
    let (metadata, metadata_reads) = db(vec![vec![]]).await;
    assert!(resolve_email_recipient(
        &source,
        &metadata,
        &proof(false, false),
        "https://source.example.invalid"
    )
    .await
    .is_err());
    assert!(source_reads.lock().unwrap().is_empty());
    let queries = metadata_reads.lock().unwrap();
    assert_eq!(queries.len(), 1);
    for invariant in [
        "principal_login_link",
        "source_employee_id",
        "source_tenant_id",
        "source_user_id",
        "installed_version",
        "runtime_status",
        "p.status='ACTIVE'",
        "l.status='VERIFIED'",
    ] {
        assert!(queries[0].contains(invariant), "missing {invariant}");
    }
}

#[tokio::test]
async fn revoked_org_binding_denied_before_address_read() {
    let (source, source_reads) = db(vec![]).await;
    let identity = ProxyRow::new(BTreeMap::from([(
        "source_user_id".into(),
        Uuid::new_v4().into(),
    )]));
    let (metadata, metadata_reads) = db(vec![vec![identity], vec![]]).await;
    assert!(resolve_email_recipient(
        &source,
        &metadata,
        &proof(true, false),
        "https://source.example.invalid"
    )
    .await
    .is_err());
    assert!(source_reads.lock().unwrap().is_empty());
    let queries = metadata_reads.lock().unwrap();
    assert_eq!(queries.len(), 2);
    assert!(queries[1].contains("b.version"));
    assert!(queries[1].contains("b.status='ACTIVE'"));
    assert!(queries[1].contains("s.expected_grant_version=b.version"));
}

#[tokio::test]
async fn ended_reporting_relationship_denied_before_address_read() {
    let (source, source_reads) = db(vec![]).await;
    let identity = ProxyRow::new(BTreeMap::from([(
        "source_user_id".into(),
        Uuid::new_v4().into(),
    )]));
    let (metadata, metadata_reads) = db(vec![vec![identity], vec![]]).await;
    assert!(resolve_email_recipient(
        &source,
        &metadata,
        &proof(true, true),
        "https://source.example.invalid"
    )
    .await
    .is_err());
    assert!(source_reads.lock().unwrap().is_empty());
    let queries = metadata_reads.lock().unwrap();
    assert_eq!(queries.len(), 2);
    for invariant in [
        "organization_relationship",
        "a.version",
        "a.installed_version=a.version",
        "a.status='ACTIVE'",
        "manager.principal_id",
    ] {
        assert!(queries[1].contains(invariant), "missing {invariant}");
    }
}
