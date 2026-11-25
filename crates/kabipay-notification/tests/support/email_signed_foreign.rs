//! Owned policy responder is an explicit external boundary, never a fabricated
//! VerifiedRuntimeAuthorization. The real source transport constructs that proof.
use super::{entity, events, execute, id, submit};
use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    routing::post,
    Json, Router,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::important_email::*;
use kabipay_db_entities::ops::{feature_flag, module, tenant, tenant_subscription};
use kabipay_notification::email::{
    destination::*, keyring::SecretString, smtp::*, store::*, templates::*, worker::*,
};
use kabipay_organization_protocol::*;
use kabipay_organization_security::*;
use kabipay_organization_tenant::{email_identity::resolve_email_recipient, email_transport::*};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use std::{
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
};
const PRIVATE: &[u8] =
    include_bytes!("../../../kabipay-organization-security/tests/fixtures/test-a-private.pem");
const PUBLIC: &[u8] =
    include_bytes!("../../../kabipay-organization-security/tests/fixtures/test-a-public.pem");

fn verifier(service: &str, audience: &str) -> ServiceCredentialVerifier {
    ServiceCredentialVerifier::new(
        audience,
        vec![ServiceVerificationKey {
            kid: "joined".into(),
            service: service.into(),
            tenant_ids: vec![id(1)],
            public_pem: PUBLIC.to_vec(),
        }],
    )
    .unwrap()
}
fn token(service: &str, audience: &str, route: &str, body: &[u8]) -> String {
    ServiceCredentialIssuer::new(service, "joined", PRIVATE)
        .unwrap()
        .issue(audience, id(1), route, body)
        .unwrap()
}
struct Backend {
    source: DatabaseConnection,
    employee: DatabaseConnection,
    sent: AtomicUsize,
}
struct Resolver;
impl SmtpResolver for Resolver {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, DestinationError> {
        Ok(vec!["8.8.8.8".parse().unwrap()])
    }
}
struct Connector<'a>(&'a AtomicUsize);
struct Session<'a>(&'a AtomicUsize);
impl<'a> SmtpConnector for Connector<'a> {
    type Session = Session<'a>;
    async fn connect(
        &self,
        _: &ValidatedSmtpDestination,
        _: SmtpTlsMode,
    ) -> Result<Self::Session, SmtpFailure> {
        Ok(Session(self.0))
    }
}
impl SmtpSession for Session<'_> {
    fn is_encrypted(&self) -> bool {
        true
    }
    fn can_starttls(&self) -> bool {
        false
    }
    async fn command(&mut self, command: SmtpCommand<'_>) -> Result<u16, SmtpFailure> {
        Ok(match command {
            SmtpCommand::Authenticate(_) => 235,
            SmtpCommand::Data => 354,
            SmtpCommand::Quit => 221,
            SmtpCommand::Body(_) => {
                self.0.fetch_add(1, Ordering::SeqCst);
                250
            }
            _ => 250,
        })
    }
}
impl DeliveryBackend for Backend {
    type Prepared = PrivateEmailRecipient;
    async fn prepare(
        &self,
        _: &DatabaseConnection,
        attempt: &DeliveryAttempt,
    ) -> Result<Self::Prepared, PreparationFailure> {
        let request = EmailEligibilityRequest {
            intent_id: attempt.id,
            event_hash: attempt.event_hash.clone(),
            event: attempt.event.clone(),
        };
        let body = serde_json::to_vec(&request).unwrap();
        let proof = verify_eligibility_transport(
            &verifier("kabipay-notification", EMAIL_ELIGIBILITY_AUDIENCE),
            &token(
                "kabipay-notification",
                EMAIL_ELIGIBILITY_AUDIENCE,
                EMAIL_ELIGIBILITY_ROUTE,
                &body,
            ),
            &request,
            &body,
            "leave",
        )
        .unwrap();
        let tx = self.source.begin().await.unwrap();
        require_persisted_email_intent(&tx, &request).await.unwrap();
        consume_email_proof(&tx, &proof, id(1)).await.unwrap();
        tx.commit().await.unwrap();
        assert_eq!(
            kabipay_leave::email_alerts::check_email_eligibility(
                &self.source,
                &DatabaseConnection::Disconnected,
                &request
            )
            .await
            .unwrap(),
            EmailEligibilityDecision::Eligible
        );
        let proof = verify_recipient_transport(
            &verifier("kabipay-leave", EMAIL_RECIPIENT_AUDIENCE),
            &token(
                "kabipay-leave",
                EMAIL_RECIPIENT_AUDIENCE,
                EMAIL_RECIPIENT_ROUTE,
                &body,
            ),
            &request,
            &body,
        )
        .unwrap();
        let tx = self.employee.begin().await.unwrap();
        consume_email_proof(&tx, proof.credential(), id(1))
            .await
            .unwrap();
        tx.commit().await.unwrap();
        let EmailRecipientResolution::Recipient { recipient } = resolve_email_recipient(
            &self.employee,
            &DatabaseConnection::Disconnected,
            &proof,
            "https://company.example.invalid",
        )
        .await
        .unwrap() else {
            panic!("current requester must resolve")
        };
        Ok(recipient)
    }
    fn identity(recipient: &Self::Prepared) -> EmailRecipientIdentity {
        recipient.identity
    }
    async fn submit(
        &self,
        db: &DatabaseConnection,
        attempt: &DeliveryAttempt,
        recipient: &Self::Prepared,
    ) -> SmtpOutcome {
        let sender = EmailSender::new(
            "Fixture".into(),
            "sender@example.invalid".into(),
            "sender@example.invalid".into(),
        )
        .unwrap();
        let message = render_email(
            &attempt.event,
            "Fixture",
            "https://company.example.invalid",
            &sender,
            recipient,
        )
        .unwrap();
        let credential = SmtpCredential::new(
            "sender@example.invalid".into(),
            SecretString::new("synthetic-only".into()).unwrap(),
        )
        .unwrap();
        let destination = resolve_smtp_destination("smtp.example.invalid", 465, &Resolver)
            .await
            .unwrap();
        send_email_attempt_with_connector(
            &Connector(&self.sent),
            &destination,
            SmtpTlsMode::Implicit,
            &credential,
            &message,
            || async {
                let current = self
                    .prepare(db, attempt)
                    .await
                    .map_err(|_| FenceError::Unavailable)?;
                if current.identity != recipient.identity
                    || current.address() != recipient.address()
                {
                    return Err(FenceError::Stale);
                }
                match mark_submitting(db, attempt).await {
                    Ok(Some(_)) => Ok(()),
                    Ok(None) => Err(FenceError::Stale),
                    Err(_) => Err(FenceError::Unavailable),
                }
            },
        )
        .await
    }
}

pub async fn run(owner: &DatabaseConnection, source: &DatabaseConnection) {
    // Create through the local writer before installing foreign review authority;
    // foreign reviewer selection itself is covered by the native metadata fixture.
    let request = submit(source, "2026-12-21").await;
    execute(owner, "CREATE SCHEMA kabipay_ops").await;
    entity(owner, tenant::Entity).await;
    entity(owner, module::Entity).await;
    entity(owner, tenant_subscription::Entity).await;
    entity(owner, feature_flag::Entity).await;
    // Existing entitlement reads are a separate owned baseline, not new mail grants.
    execute(owner,r#"DO $$ DECLARE c record;v text;BEGIN FOR c IN SELECT table_name,column_name,data_type FROM information_schema.columns WHERE table_schema='kabipay_ops' AND is_nullable='NO' AND column_default IS NULL LOOP v:=CASE c.data_type WHEN 'uuid' THEN 'gen_random_uuid()' WHEN 'boolean' THEN 'false' WHEN 'text' THEN quote_literal('fixture') WHEN 'character varying' THEN quote_literal('fixture') WHEN 'integer' THEN '0' WHEN 'bigint' THEN '0' WHEN 'timestamp with time zone' THEN 'clock_timestamp()' ELSE NULL END;IF v IS NOT NULL THEN EXECUTE format('ALTER TABLE kabipay_ops.%I ALTER COLUMN %I SET DEFAULT %s',c.table_name,c.column_name,v);END IF;END LOOP;END $$;CREATE TABLE kabipay_ops.module_dependency(module_id uuid,depends_on_module_id uuid);"#).await;
    execute(owner,&format!(r#"INSERT INTO kabipay_ops.tenant(id,name,status,is_deleted,timezone) VALUES('{}','Fixture','ACTIVE',false,'UTC');INSERT INTO kabipay_ops.module(id,code,name,is_active,is_core) VALUES('{}','LEAVE','Leave',true,true);
    CREATE TABLE actor_ref(tenant_id uuid,principal_id uuid,display_name text,identity_version bigint,last_verified_at timestamptz,PRIMARY KEY(tenant_id,principal_id));
    CREATE TABLE organization_resource_version(tenant_id uuid,resource_type text,resource_id uuid,version bigint,PRIMARY KEY(tenant_id,resource_type,resource_id));
    CREATE TABLE organization_command_receipt(tenant_id uuid,command_id uuid,principal_id uuid,source_tenant_id uuid,source_user_id uuid,organization_id uuid,membership_id uuid,authority_id uuid,authority_version bigint,resource_id uuid,operation text,request_hash text,expected_version bigint,outcome jsonb,PRIMARY KEY(tenant_id,command_id));
    CREATE TABLE organization_business_actor(tenant_id uuid,resource_type text,resource_id uuid,principal_id uuid,command_id uuid,action text);
    GRANT SELECT,INSERT,UPDATE ON actor_ref,organization_resource_version,organization_command_receipt,organization_business_actor TO hrms_company_leave;
    GRANT UPDATE(tenant_id) ON organization_membership_installation,organization_grant_installation TO hrms_company_leave;
    INSERT INTO organization_membership_installation(tenant_id,organization_id,membership_id,version,status) VALUES('{}','{}','{}',1,'ACTIVE');
    INSERT INTO organization_grant_installation(tenant_id,binding_id,organization_id,membership_id,principal_id,grant_version,membership_version,status,capabilities) VALUES('{}','{}','{}','{}','{}',1,1,'ACTIVE','[{{"code":"leave:approve","employee_scope":"ALL"}}]');
    INSERT INTO actor_ref VALUES('{}','{}','Foreign reviewer',1,clock_timestamp());
    UPDATE important_email_intent SET state='SKIPPED',terminal_at=clock_timestamp() WHERE state='PENDING';
    UPDATE tenant_email_settings SET version=version+1,enabled=true,enabled_after=clock_timestamp(),category_cutoffs=jsonb_build_object('leave',clock_timestamp()),smtp_host='smtp.example.invalid',sender_email='sender@example.invalid',account_email='sender@example.invalid',sender_name='Fixture',categories='{{"leave":true,"approval":false,"payslip":false,"missed_punch_out":false}}',credential_cipher=decode(repeat('aa',32),'hex'),credential_nonce=decode(repeat('bb',12),'hex'),credential_key_id='synthetic';"#,id(1),id(399),id(1),id(400),id(401),id(1),id(402),id(400),id(401),id(403),id(1),id(403))).await;
    execute(
        owner,
        &format!(
            "INSERT INTO organization_resource_version VALUES('{}','leave_request','{}',1)",
            id(1),
            request.id
        ),
    )
    .await;
    let command = OrganizationCommand {
        command_id: id(410),
        resource_id: request.id,
        expected_version: 1,
        operation: OrganizationOperation::LeaveDecision,
        payload: serde_json::json!({"decision":"reject","expected_workflow_step_id":id(31),"rejection_reason":null}),
    };
    let body = serde_json::to_vec(&command).unwrap();
    let now = chrono::Utc::now().timestamp() as u64;
    let claims:DelegationClaims=serde_json::from_value(serde_json::json!({"iss":DELEGATION_ISSUER,"aud":"kabipay-leave","sub":id(403),"jti":id(411),"iat":now,"nbf":now,"exp":now+300,"organization_id":id(400),"target_membership_id":id(401),"source_tenant_id":id(404),"source_user_id":id(405),"target_tenant_id":id(1),"identity_version":1,"source_link_version":1,"target_membership_version":1,"authority_kind":"organization_grant","authority_ref":id(402),"authority_version":1,"capability":"leave:approve","resource_type":"leave_request","resource_id":request.id,"expected_resource_version":1,"command_id":command.command_id,"request_hash":command_body_hash(&body)})).unwrap();
    let delegation = DelegationIssuer::new("joined", PRIVATE)
        .unwrap()
        .issue(&claims)
        .unwrap();
    let policy:RuntimeAuthorization=serde_json::from_value(serde_json::json!({"organization_id":id(400),"membership_id":id(401),"tenant_id":id(1),"principal_id":id(403),"source_tenant_id":id(404),"source_user_id":id(405),"identity_version":1,"source_link_version":1,"membership_version":1,"authority_kind":"organization_grant","authority_id":id(402),"authority_version":1,"capability":"leave:approve","employee_scope":"ALL","employee_id":id(2),"actor_target_user_ids":[],"actor":{"principal_id":id(403),"display_name":"Foreign reviewer","identity_version":1},"grant":null,"relationship":null,"membership":null})).unwrap();
    let expected_body = STANDARD.encode(&body);
    let expected_delegation = delegation.clone();
    // The external policy responder authenticates the source proof and exact
    // command. Actual source installation/membership/version locks still run.
    let router = Router::new().route(
        "/internal/organization/policy",
        post(move |headers: HeaderMap, body: Bytes| {
            let policy = policy.clone();
            let expected_body = expected_body.clone();
            let expected_delegation = expected_delegation.clone();
            async move {
                let bearer = headers
                    .get("authorization")
                    .and_then(|v| v.to_str().ok())
                    .and_then(|v| v.strip_prefix("Bearer "))
                    .ok_or(StatusCode::UNAUTHORIZED)?;
                verifier("kabipay-leave", "kabipay-organization")
                    .verify(bearer, id(1), "/internal/organization/policy", &body)
                    .map_err(|_| StatusCode::UNAUTHORIZED)?;
                let request: RuntimePolicyRequest =
                    serde_json::from_slice(&body).map_err(|_| StatusCode::BAD_REQUEST)?;
                if request.employee_id != Some(id(2))
                    || request.command_body_base64 != expected_body
                    || request.delegation_token != expected_delegation
                {
                    return Err(StatusCode::FORBIDDEN);
                }
                Ok::<_, StatusCode>(Json(policy))
            }
        }),
    );
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let server = tokio::spawn(async move { axum::serve(listener, router).await.unwrap() });
    let key_root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../kabipay-organization-security/tests/fixtures")
        .canonicalize()
        .unwrap();
    for (name, value) in [
        ("KABIPAY_SERVICE_ID", "kabipay-leave".into()),
        ("KABIPAY_SERVICE_KEY_ID", "joined".into()),
        (
            "KABIPAY_SERVICE_PRIVATE_KEY_FILE",
            key_root.join("test-a-private.pem").to_str().unwrap().into(),
        ),
        (
            "KABIPAY_ORGANIZATION_DELEGATION_PUBLIC_KEYS",
            serde_json::json!({"joined":key_root.join("test-a-public.pem")}).to_string(),
        ),
        (
            "KABIPAY_ORGANIZATION_POLICY_URL",
            format!("http://{address}"),
        ),
    ] {
        std::env::set_var(name, value)
    }
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        format!("Bearer {delegation}").parse().unwrap(),
    );
    let before = events(owner, request.id).await;
    let mut forged = body.clone();
    forged.push(b' ');
    assert!(kabipay_organization_tenant::run(
        &kabipay_leave::organization::LeaveAdapter,
        id(1),
        source,
        owner,
        &headers,
        &forged
    )
    .await
    .is_err());
    assert_eq!(events(owner, request.id).await.len(), before.len());
    super::owned::inject_intent_failure(owner, true).await;
    assert!(kabipay_organization_tenant::run(
        &kabipay_leave::organization::LeaveAdapter,
        id(1),
        source,
        owner,
        &headers,
        &body
    )
    .await
    .is_err());
    assert_eq!(
        events(owner, request.id).await.len(),
        before.len(),
        "failed intent rolls back signed business decision"
    );
    super::owned::inject_intent_failure(owner, false).await;
    let outcome = kabipay_organization_tenant::run(
        &kabipay_leave::organization::LeaveAdapter,
        id(1),
        source,
        owner,
        &headers,
        &body,
    )
    .await
    .unwrap();
    assert_eq!(outcome.status, "COMMITTED");
    let committed = events(owner, request.id).await;
    assert_eq!(committed.len(), before.len() + 1);
    assert!(committed
        .iter()
        .any(|(_, event, _)| event.kind == EmailEventKind::Outcome
            && event.actor_ref
                == Some(EmailActorRef::OrganizationPrincipal {
                    principal_id: id(403)
                })));
    kabipay_organization_tenant::run(
        &kabipay_leave::organization::LeaveAdapter,
        id(1),
        source,
        owner,
        &headers,
        &body,
    )
    .await
    .unwrap();
    assert_eq!(events(owner, request.id).await.len(), committed.len());
    execute(owner,"UPDATE important_email_intent SET state='SKIPPED',terminal_at=clock_timestamp() WHERE state='PENDING' AND kind='action_required'").await;
    let fixture = super::owned::OwnedEmailFixture::load();
    let notification = fixture.connect("hrms_company_notification").await;
    let attempts = claim_email_batch(&notification, id(1), 2).await.unwrap();
    assert_eq!(attempts.len(), 1);
    let backend = Backend {
        source: source.clone(),
        employee: fixture.connect("hrms_company_employee").await,
        sent: AtomicUsize::new(0),
    };
    let (_stop, shutdown) = tokio::sync::watch::channel(false);
    process_attempt(&notification, &attempts[0], &backend, shutdown.clone())
        .await
        .unwrap();
    let state: String = owner
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT state FROM important_email_intent WHERE id=$1",
            [attempts[0].id.into()],
        ))
        .await
        .unwrap()
        .unwrap()
        .try_get("", "state")
        .unwrap();
    assert_eq!(state, "ACCEPTED");
    process_attempt(&notification, &attempts[0], &backend, shutdown)
        .await
        .unwrap();
    assert_eq!(
        backend.sent.load(Ordering::SeqCst),
        1,
        "a replayed lease cannot send again"
    );
    server.abort();
}
