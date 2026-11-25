#[path = "support/email_message.rs"]
mod message;
#[path = "support/email_smtp_server.rs"]
mod server;
use base64::Engine;
use kabipay_notification::email::{
    destination::{
        resolve_smtp_destination, DestinationError, SmtpResolver, ValidatedSmtpDestination,
    },
    keyring::SecretString,
    smtp::{
        send_email_attempt_with_connector, FenceError, LettreSession, SmtpConnector,
        SmtpCredential, SmtpFailure, SmtpOutcome, SmtpTlsMode,
    },
    templates::{render_saved_account_test, EmailSender},
};
use lettre::transport::smtp::{
    client::{AsyncSmtpConnection, Certificate, TlsParameters},
    extension::ClientId,
};
use server::Behavior;
use std::{
    io::Write,
    net::{IpAddr, SocketAddr},
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Arc, Mutex,
    },
    time::Duration,
};
use tracing::instrument::WithSubscriber;
#[derive(Clone, Default)]
struct Capture(Arc<Mutex<Vec<u8>>>);
struct CaptureWriter(Arc<Mutex<Vec<u8>>>);
impl Write for CaptureWriter {
    fn write(&mut self, value: &[u8]) -> std::io::Result<usize> {
        self.0.lock().unwrap().extend_from_slice(value);
        Ok(value.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for Capture {
    type Writer = CaptureWriter;
    fn make_writer(&'a self) -> CaptureWriter {
        CaptureWriter(self.0.clone())
    }
}
struct Resolver(AtomicUsize);
impl SmtpResolver for Resolver {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, DestinationError> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Ok(vec!["8.8.8.8".parse().unwrap()])
    }
}
struct LoopbackFixtureConnector {
    address: SocketAddr,
    trust_fixture: bool,
}
impl SmtpConnector for LoopbackFixtureConnector {
    type Session = LettreSession;
    async fn connect(
        &self,
        destination: &ValidatedSmtpDestination,
        mode: SmtpTlsMode,
    ) -> Result<LettreSession, SmtpFailure> {
        // Only this test connector reroutes the validated logical public socket to
        // its owned loopback listener. Production has no reroute/CA override setting.
        assert_eq!(
            destination.socket_addr().ip(),
            "8.8.8.8".parse::<IpAddr>().unwrap()
        );
        assert!(self.address.ip().is_loopback());
        let mut tls = TlsParameters::builder(destination.hostname().to_owned());
        if self.trust_fixture {
            tls = tls.add_root_certificate(
                Certificate::from_pem(include_bytes!("fixtures/smtp-test-ca.pem")).unwrap(),
            );
        }
        let tls = tls.build_rustls().map_err(|_| SmtpFailure::Tls)?;
        let connection = AsyncSmtpConnection::connect_tokio1(
            self.address,
            Some(Duration::from_secs(3)),
            &ClientId::default(),
            if mode == SmtpTlsMode::Implicit {
                Some(tls.clone())
            } else {
                None
            },
            None,
        )
        .await
        .map_err(SmtpFailure::from_smtp_error)?;
        Ok(LettreSession::from_connection(connection, tls))
    }
}
async fn attempt(
    behavior: Behavior,
    starttls: bool,
    hostname: &str,
    trust_fixture: bool,
) -> (SmtpOutcome, Vec<&'static str>) {
    let fence = Arc::new(AtomicBool::new(false));
    let server = server::start(starttls, behavior, fence.clone()).await;
    let resolver = Resolver(AtomicUsize::new(0));
    let destination =
        resolve_smtp_destination(hostname, if starttls { 587 } else { 465 }, &resolver)
            .await
            .unwrap();
    let connector = LoopbackFixtureConnector {
        address: server.address,
        trust_fixture,
    };
    let sender = EmailSender::new(
        "Synthetic".into(),
        "sender@example.invalid".into(),
        "saved@example.invalid".into(),
    )
    .unwrap();
    let rendered = render_saved_account_test(
        &message::event(),
        "Synthetic",
        "https://company.example.invalid",
        &sender,
    )
    .unwrap();
    let credential = SmtpCredential::new(
        "saved@example.invalid".into(),
        SecretString::new("test-not-real-app-password".into()).unwrap(),
    )
    .unwrap();
    let captured = Capture::default();
    let subscriber = tracing_subscriber::fmt()
        .without_time()
        .with_ansi(false)
        .with_max_level(tracing::Level::TRACE)
        .with_writer(captured.clone())
        .finish();
    let outcome = tokio::time::timeout(
        Duration::from_secs(5),
        send_email_attempt_with_connector(
            &connector,
            &destination,
            if starttls {
                SmtpTlsMode::RequiredStartTls
            } else {
                SmtpTlsMode::Implicit
            },
            &credential,
            &rendered,
            || async move {
                if matches!(behavior, Behavior::FenceRefused) {
                    return Err(FenceError::Stale);
                }
                fence.store(true, Ordering::Release);
                Ok(())
            },
        ),
    )
    .with_subscriber(subscriber)
    .await
    .unwrap();
    let log = tokio::time::timeout(Duration::from_secs(5), server.finished)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(
        resolver.0.load(Ordering::SeqCst),
        1,
        "no DNS re-resolution at connect/submission"
    );
    let visible = format!("{outcome:?}");
    for secret in [
        "private-provider-text",
        "test-not-real-app-password",
        "saved@example.invalid",
    ] {
        assert!(!visible.contains(secret));
    }
    let logged = String::from_utf8(captured.0.lock().unwrap().clone()).unwrap();
    for private in [
        "private-provider-text".to_owned(),
        "test-not-real-app-password".to_owned(),
        "saved@example.invalid".to_owned(),
        "This is a test of".to_owned(),
        base64::engine::general_purpose::STANDARD
            .encode(b"\0saved@example.invalid\0test-not-real-app-password"),
    ] {
        assert!(
            !logged.contains(&private),
            "SMTP logs contain only fixed categories"
        );
    }
    (outcome, log)
}
#[tokio::test]
async fn real_tls_certificate_hostname_and_required_starttls_are_enforced() {
    for starttls in [false, true] {
        let (outcome, log) =
            attempt(Behavior::Accept, starttls, "smtp.example.invalid", true).await;
        assert_eq!(outcome, SmtpOutcome::Accepted);
        assert!(log.contains(&"body"));
    }
    for (host, trust) in [
        ("smtp.wrong.invalid", true),
        ("smtp.example.invalid", false),
    ] {
        let (outcome, log) = attempt(Behavior::Accept, false, host, trust).await;
        assert!(matches!(outcome, SmtpOutcome::Rejected(_)), "{outcome:?}");
        assert!(!log.contains(&"auth"));
        assert!(!log.contains(&"body"));
    }
    let (outcome, log) = attempt(
        Behavior::MissingStartTls,
        true,
        "smtp.example.invalid",
        true,
    )
    .await;
    assert!(matches!(outcome, SmtpOutcome::Rejected(_)));
    assert!(!log.contains(&"auth"));
}
#[tokio::test]
async fn real_wire_responses_preserve_submission_uncertainty_and_fence_order() {
    for behavior in [
        Behavior::AuthenticationRejected,
        Behavior::NegativeData,
        Behavior::PermanentBody,
    ] {
        assert!(matches!(
            attempt(behavior, false, "smtp.example.invalid", true)
                .await
                .0,
            SmtpOutcome::Rejected(_)
        ));
    }
    for behavior in [Behavior::TemporaryEnvelope, Behavior::TemporaryBody] {
        assert!(matches!(
            attempt(behavior, true, "smtp.example.invalid", true)
                .await
                .0,
            SmtpOutcome::RetrySafe(_)
        ));
    }
    assert!(matches!(
        attempt(
            Behavior::LostAcknowledgement,
            false,
            "smtp.example.invalid",
            true
        )
        .await
        .0,
        SmtpOutcome::Uncertain(_)
    ));
    assert_eq!(
        attempt(Behavior::QuitLost, false, "smtp.example.invalid", true)
            .await
            .0,
        SmtpOutcome::Accepted
    );
    let (outcome, log) = attempt(Behavior::FenceRefused, true, "smtp.example.invalid", true).await;
    assert!(matches!(outcome, SmtpOutcome::NotSubmitted(_)));
    assert!(!log.contains(&"body"));
}
