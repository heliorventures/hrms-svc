//! Real Notification SQL contracts; only the owned-loopback launcher is accepted.
#[path = "../../kabipay-common/tests/support/owned_email.rs"]
mod owned;
use chrono::{Duration, Utc};
use kabipay_common::important_email::*;
use kabipay_notification::email::worker::process_attempt;
use kabipay_notification::email::{
    smtp::{SmtpErrorCode, SmtpOutcome},
    store::*,
};
use owned::*;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "support/email_delivery_backend.rs"]
mod backend;
use backend::FakeBackend;
fn sql(q: &str, v: Vec<sea_orm::Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, q, v)
}
async fn event(db: &DatabaseConnection, n: u32, foreign: bool) {
    let now = Utc::now();
    let tenant = id(1);
    let resource = id(1000 + n);
    let recipient = if foreign {
        EmailRecipientRef::OrganizationPrincipal {
            principal_id: id(8),
            source_tenant_id: id(1),
            source_employee_id: id(2),
        }
    } else {
        EmailRecipientRef::LocalUser { user_id: id(5) }
    };
    let authorization_ref = if foreign {
        EmailAuthorizationRef::OrganizationGrant {
            organization_id: id(21),
            membership_id: id(22),
            binding_id: id(23),
            version: 1,
        }
    } else {
        EmailAuthorizationRef::DomainTransition {
            resource_type: EmailResourceType::Leave,
            transition_key: "approved:1".into(),
        }
    };
    let action_target = if foreign {
        EmailActionTarget::OrganizationResource {
            organization_id: id(21),
            membership_id: id(22),
            resource_type: EmailResourceType::Leave,
            resource_id: resource,
        }
    } else {
        EmailActionTarget::CompanyResource {
            resource_type: EmailResourceType::Leave,
            resource_id: resource,
        }
    };
    let tx = db.begin().await.unwrap();
    append_email_intent(
        &tx,
        &EmailEvent {
            event_id: id(n),
            tenant_id: tenant,
            resource_type: EmailResourceType::Leave,
            resource_id: resource,
            resource_version: "approved:1".into(),
            category: EmailCategory::Leave,
            kind: EmailEventKind::Outcome,
            template_version: 1,
            recipient,
            actor_ref: None,
            authorization_ref,
            action_target,
            occurred_at: now,
            expires_at: now + Duration::days(1),
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
}
#[tokio::test]
#[ignore = "Node-owned child intentionally terminated after its durable phase"]
async fn owned_email_crash_child() {
    let mode = std::env::var("KABIPAY_EMAIL_CRASH_MODE").unwrap();
    assert!(matches!(mode.as_str(), "predata" | "submitting"));
    let fixture = OwnedEmailFixture::load();
    let owner = fixture.connect("fixture_owner").await;
    execute(&owner, "CREATE SCHEMA tenant_fixture").await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_leave").await;
    event(&source, 9000, false).await;
    let notification = fixture.connect("hrms_company_notification").await;
    let attempt = claim_email_batch(&notification, id(1), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let backend = FakeBackend {
        block_body: mode == "submitting",
        block_prepare: mode == "predata",
        ..FakeBackend::default()
    };
    let (_stop, shutdown) = tokio::sync::watch::channel(false);
    process_attempt(&notification, &attempt, &backend, shutdown)
        .await
        .unwrap();
    panic!("parent must terminate this synthetic child before its phase deadline");
}
#[tokio::test]
#[ignore = "Node-owned separate-process recovery only"]
async fn owned_email_crash_readback() {
    let mode = std::env::var("KABIPAY_EMAIL_CRASH_MODE").unwrap();
    assert!(matches!(mode.as_str(), "predata" | "submitting"));
    let fixture = OwnedEmailFixture::load();
    let owner = fixture.connect("fixture_owner").await;
    let notification = fixture.connect("hrms_company_notification").await;
    assert!(claim_email_batch(&notification, id(1), 1)
        .await
        .unwrap()
        .is_empty());
    let row = owner
        .query_one(sql(
            "SELECT state,attempt_count FROM important_email_intent WHERE event_id=$1",
            vec![id(9000).into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let current: String = row.try_get("", "state").unwrap();
    assert_eq!(
        current,
        if mode == "predata" {
            "PENDING"
        } else {
            "UNCERTAIN"
        }
    );
    let retries: i32 = row.try_get("", "attempt_count").unwrap();
    assert_eq!(retries, if mode == "predata" { 1 } else { 0 });
}
async fn state(db: &DatabaseConnection, id: uuid::Uuid) -> String {
    db.query_one(sql(
        "SELECT state FROM important_email_intent WHERE id=$1",
        vec![id.into()],
    ))
    .await
    .unwrap()
    .unwrap()
    .try_get("", "state")
    .unwrap()
}
#[tokio::test]
#[ignore = "owned Node loopback fixture only"]
async fn owned_email_delivery_state_transitions() {
    let fixture = OwnedEmailFixture::load();
    let owner = fixture.connect("fixture_owner").await;
    execute(&owner, "CREATE SCHEMA tenant_fixture").await;
    fixture.install_owned_sql(&owner).await;
    let source = fixture.connect("hrms_company_leave").await;
    let first = fixture.connect("hrms_company_notification").await;
    let second = fixture.connect("hrms_company_notification").await;
    event(&source, 40, false).await;
    event(&source, 41, false).await;
    event(&source, 42, false).await;
    // two_workers_cannot_submit_same_lease: real concurrent pools, exact login.
    let (a, b, c) = tokio::join!(
        claim_email_batch(&first, id(1), 20),
        claim_email_batch(&second, id(1), 20),
        claim_email_batch(&first, id(1), 20)
    );
    let mut attempts = a.unwrap();
    attempts.extend(b.unwrap());
    attempts.extend(c.unwrap());
    assert_eq!(attempts.len(), 2);
    assert_ne!(attempts[0].id, attempts[1].id);
    let identity = EmailRecipientIdentity {
        source_tenant_id: id(1),
        source_user_id: id(5),
    };
    assert_eq!(
        reserve_identity(&first, &attempts[0], &identity)
            .await
            .unwrap(),
        IdentityReservation::Ready
    );
    assert!(mark_submitting(&first, &attempts[0])
        .await
        .unwrap()
        .is_some());
    assert!(
        mark_submitting(&second, &attempts[0])
            .await
            .unwrap()
            .is_none(),
        "same lease cannot submit twice"
    );
    assert!(
        complete_email_attempt(&first, &attempts[0], SmtpOutcome::Accepted)
            .await
            .unwrap()
    );
    assert!(
        !complete_email_attempt(&second, &attempts[0], SmtpOutcome::Accepted)
            .await
            .unwrap(),
        "stale_token_cannot_complete"
    );
    // transport_change_fences_unstarted_attempt.
    execute(&owner, "UPDATE tenant_email_settings SET version=version+1").await;
    assert!(mark_submitting(&first, &attempts[1])
        .await
        .unwrap()
        .is_none());
    assert!(!heartbeat(&first, &attempts[1]).await.unwrap());
    assert!(
        skip_email_attempt(&first, &attempts[1], DeliveryCode::SettingsChanged)
            .await
            .unwrap()
    );
    let third = claim_email_batch(&first, id(1), 20)
        .await
        .unwrap()
        .pop()
        .unwrap();
    skip_email_attempt(&first, &third, DeliveryCode::Disabled)
        .await
        .unwrap();
    // local_and_org_same_recipient_send_once: one event, two routing aliases.
    event(&source, 50, false).await;
    event(&source, 50, true).await;
    let aliases = claim_email_batch(&first, id(1), 20).await.unwrap();
    assert_eq!(aliases.len(), 2);
    assert_eq!(
        reserve_identity(&first, &aliases[0], &identity)
            .await
            .unwrap(),
        IdentityReservation::Ready
    );
    assert_eq!(
        reserve_identity(&second, &aliases[1], &identity)
            .await
            .unwrap(),
        IdentityReservation::Duplicate
    );
    assert!(mark_submitting(&first, &aliases[0])
        .await
        .unwrap()
        .is_some());
    complete_email_attempt(&first, &aliases[0], SmtpOutcome::Accepted)
        .await
        .unwrap();
    assert_eq!(state(&owner, aliases[1].id).await, "SKIPPED");
    // A safe negative response advances the retry counter and cannot be reclaimed immediately.
    event(&source, 60, false).await;
    let retry = claim_email_batch(&first, id(1), 20)
        .await
        .unwrap()
        .pop()
        .unwrap();
    reserve_identity(&first, &retry, &identity).await.unwrap();
    mark_submitting(&first, &retry).await.unwrap().unwrap();
    assert!(complete_email_attempt(
        &first,
        &retry,
        SmtpOutcome::RetrySafe(SmtpErrorCode::Unavailable)
    )
    .await
    .unwrap());
    assert_eq!(state(&owner, retry.id).await, "PENDING");
    assert!(claim_email_batch(&second, id(1), 20)
        .await
        .unwrap()
        .is_empty());
    execute(&owner,"UPDATE important_email_intent SET state='SKIPPED',terminal_at=clock_timestamp() WHERE state='PENDING'").await;
    // The real worker path respects revocation before its network submission.
    event(&source, 65, false).await;
    let revoked = claim_email_batch(&first, id(1), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let revoked_backend = FakeBackend {
        sent: AtomicUsize::new(0),
        revoked: true,
        ..FakeBackend::default()
    };
    let (_stop, shutdown) = tokio::sync::watch::channel(false);
    process_attempt(&first, &revoked, &revoked_backend, shutdown)
        .await
        .unwrap();
    assert_eq!(
        state(&owner, revoked.id).await,
        "SKIPPED",
        "test_revocation_before_send_denied"
    );
    assert_eq!(revoked_backend.sent.load(Ordering::Relaxed), 0);
    // Competing orchestration futures with the same lease still submit once.
    event(&source, 66, false).await;
    let same = claim_email_batch(&first, id(1), 1)
        .await
        .unwrap()
        .pop()
        .unwrap();
    let backend = FakeBackend {
        sent: AtomicUsize::new(0),
        revoked: false,
        ..FakeBackend::default()
    };
    let (_stop, shutdown) = tokio::sync::watch::channel(false);
    let (one, two) = tokio::join!(
        process_attempt(&first, &same, &backend, shutdown.clone()),
        process_attempt(&second, &same, &backend, shutdown)
    );
    one.unwrap();
    two.unwrap();
    assert_eq!(backend.sent.load(Ordering::Relaxed), 1);
    assert_eq!(state(&owner, same.id).await, "ACCEPTED");
    // Real 90-second lease expiry: do not disable the guard or fake the worker clock.
    event(&source, 70, false).await;
    event(&source, 71, false).await;
    let crashed = claim_email_batch(&first, id(1), 20).await.unwrap();
    assert_eq!(crashed.len(), 2);
    // Inject failure only after simulated SMTP acceptance; the real fence stays.
    execute(&owner,"CREATE FUNCTION fixture_fail_accept() RETURNS trigger LANGUAGE plpgsql AS $$ BEGIN IF NEW.state='ACCEPTED' THEN RAISE EXCEPTION 'synthetic completion unavailable';END IF;RETURN NEW;END $$;CREATE TRIGGER fixture_fail_accept BEFORE UPDATE ON important_email_intent FOR EACH ROW EXECUTE FUNCTION fixture_fail_accept()").await;
    let accepted_backend = FakeBackend {
        sent: AtomicUsize::new(0),
        revoked: false,
        ..FakeBackend::default()
    };
    let (_stop, shutdown) = tokio::sync::watch::channel(false);
    assert!(
        process_attempt(&first, &crashed[0], &accepted_backend, shutdown)
            .await
            .is_err()
    );
    assert_eq!(accepted_backend.sent.load(Ordering::Relaxed), 1);
    assert_eq!(state(&owner, crashed[0].id).await, "SUBMITTING");
    execute(&owner,"DROP TRIGGER fixture_fail_accept ON important_email_intent;DROP FUNCTION fixture_fail_accept()").await;
    tokio::time::sleep(std::time::Duration::from_secs(91)).await;
    assert!(claim_email_batch(&second, id(1), 20)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        state(&owner, crashed[0].id).await,
        "UNCERTAIN",
        "crash_submitting_becomes_uncertain / accepted_but_db_ack_lost_not_resent"
    );
    assert_eq!(
        state(&owner, crashed[1].id).await,
        "PENDING",
        "crash_predata_retries_safely"
    );
    assert!(!heartbeat(&first, &crashed[1]).await.unwrap());
    assert!(
        !complete_email_attempt(&first, &crashed[0], SmtpOutcome::Accepted)
            .await
            .unwrap()
    );
    // Current terminal metadata cannot be purged, even with raw DELETE privilege.
    assert_eq!(purge_terminal(&first, id(1)).await.unwrap(), 0);
    assert!(
        first
            .execute(sql("DELETE FROM important_email_intent", vec![]))
            .await
            .is_err(),
        "terminal_purge_keeps_live_or_new_metadata"
    );
}
