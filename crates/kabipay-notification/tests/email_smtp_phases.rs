#[path = "support/email_message.rs"]
mod support;
use kabipay_notification::email::{
    destination::{
        resolve_smtp_destination, DestinationError, SmtpResolver, ValidatedSmtpDestination,
    },
    keyring::SecretString,
    smtp::{
        send_email_attempt_with_connector, FenceError, SmtpCommand, SmtpConnector, SmtpCredential,
        SmtpFailure, SmtpOutcome, SmtpSession, SmtpTlsMode,
    },
    templates::{render_saved_account_test, EmailSender},
};
use std::{
    net::IpAddr,
    sync::{Arc, Mutex},
    time::Duration,
};
struct Resolver;
impl SmtpResolver for Resolver {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, DestinationError> {
        Ok(vec!["8.8.8.8".parse().unwrap()])
    }
}
#[derive(Clone, Copy)]
enum Script {
    Accept,
    MissingTls,
    AuthFailed,
    WrongAuth,
    BeforeTemporary,
    DataRejected,
    BodyTemporary,
    LostAck,
    QuitFailed,
    HangBefore,
    HangBody,
    WrongData,
    WrongFinal,
}
struct Connector {
    script: Script,
    log: Arc<Mutex<Vec<&'static str>>>,
}
struct Session {
    script: Script,
    log: Arc<Mutex<Vec<&'static str>>>,
    encrypted: bool,
}
impl SmtpConnector for Connector {
    type Session = Session;
    async fn connect(
        &self,
        destination: &ValidatedSmtpDestination,
        mode: SmtpTlsMode,
    ) -> Result<Session, SmtpFailure> {
        assert_eq!(destination.hostname(), "smtp.example.invalid");
        assert_eq!(
            destination.socket_addr().ip(),
            "8.8.8.8".parse::<IpAddr>().unwrap()
        );
        self.log.lock().unwrap().push("connect");
        if matches!(self.script, Script::HangBefore) {
            tokio::time::sleep(Duration::from_secs(31)).await;
        }
        Ok(Session {
            script: self.script,
            log: self.log.clone(),
            encrypted: mode == SmtpTlsMode::Implicit,
        })
    }
}
impl SmtpSession for Session {
    fn is_encrypted(&self) -> bool {
        self.encrypted
    }
    fn can_starttls(&self) -> bool {
        !matches!(self.script, Script::MissingTls)
    }
    async fn command(&mut self, command: SmtpCommand<'_>) -> Result<u16, SmtpFailure> {
        let label = command.label();
        self.log.lock().unwrap().push(label);
        match command {
            SmtpCommand::StartTls => {
                self.encrypted = true;
                Ok(220)
            }
            SmtpCommand::Authenticate(_) => {
                assert!(self.encrypted, "no credentials on plaintext transport");
                if matches!(self.script, Script::AuthFailed) {
                    Err(SmtpFailure::Permanent)
                } else if matches!(self.script, Script::WrongAuth) {
                    Ok(250)
                } else {
                    Ok(235)
                }
            }
            SmtpCommand::Mail(_) => {
                if matches!(self.script, Script::BeforeTemporary) {
                    Err(SmtpFailure::Temporary)
                } else {
                    Ok(250)
                }
            }
            SmtpCommand::Recipient(_) => Ok(250),
            SmtpCommand::Data => {
                if matches!(self.script, Script::DataRejected) {
                    Err(SmtpFailure::Permanent)
                } else if matches!(self.script, Script::WrongData) {
                    Ok(250)
                } else {
                    Ok(354)
                }
            }
            SmtpCommand::Body(_) => {
                if matches!(self.script, Script::HangBody) {
                    tokio::time::sleep(Duration::from_secs(31)).await;
                }
                match self.script {
                    Script::BodyTemporary => Err(SmtpFailure::Temporary),
                    Script::LostAck => Err(SmtpFailure::Disconnected),
                    Script::WrongFinal => Ok(354),
                    _ => Ok(250),
                }
            }
            SmtpCommand::Quit => {
                if matches!(self.script, Script::QuitFailed) {
                    Err(SmtpFailure::Disconnected)
                } else {
                    Ok(221)
                }
            }
        }
    }
}
async fn attempt(
    script: Script,
    port: u16,
    fence: Result<(), FenceError>,
) -> (SmtpOutcome, Vec<&'static str>) {
    let destination = resolve_smtp_destination("smtp.example.invalid", port, &Resolver)
        .await
        .unwrap();
    let sender = EmailSender::new(
        "Synthetic company".into(),
        "sender@example.invalid".into(),
        "saved@example.invalid".into(),
    )
    .unwrap();
    let message = render_saved_account_test(
        &support::event(),
        "Synthetic company",
        "https://company.example.invalid",
        &sender,
    )
    .unwrap();
    let credential = SmtpCredential::new(
        "saved@example.invalid".into(),
        SecretString::new("test-not-real-app-password".into()).unwrap(),
    )
    .unwrap();
    let log = Arc::new(Mutex::new(vec![]));
    let connector = Connector {
        script,
        log: log.clone(),
    };
    let gate_log = log.clone();
    let outcome = send_email_attempt_with_connector(
        &connector,
        &destination,
        if port == 465 {
            SmtpTlsMode::Implicit
        } else {
            SmtpTlsMode::RequiredStartTls
        },
        &credential,
        &message,
        || async move {
            gate_log.lock().unwrap().push("fence");
            fence
        },
    )
    .await;
    let recorded = log.lock().unwrap().clone();
    (outcome, recorded)
}
#[tokio::test]
async fn submitting_fence_is_confirmed_after_data_acceptance_and_before_any_body() {
    for port in [465, 587] {
        let (outcome, log) = attempt(Script::Accept, port, Ok(())).await;
        assert_eq!(outcome, SmtpOutcome::Accepted);
        assert!(
            log.iter().position(|v| *v == "data").unwrap()
                < log.iter().position(|v| *v == "fence").unwrap()
        );
        assert!(
            log.iter().position(|v| *v == "fence").unwrap()
                < log.iter().position(|v| *v == "body").unwrap()
        );
    }
    let (outcome, log) = attempt(Script::Accept, 587, Err(FenceError::Stale)).await;
    assert!(matches!(outcome, SmtpOutcome::NotSubmitted(_)));
    assert!(!log.contains(&"body"));
    assert!(
        !log.contains(&"quit"),
        "QUIT would become message content while DATA waits"
    );
}
#[tokio::test]
async fn missing_starttls_and_authentication_failure_never_submit_a_body() {
    for script in [
        Script::MissingTls,
        Script::AuthFailed,
        Script::WrongAuth,
        Script::DataRejected,
        Script::WrongData,
    ] {
        let (outcome, log) = attempt(script, 587, Ok(())).await;
        assert!(matches!(outcome, SmtpOutcome::Rejected(_)));
        assert!(!log.contains(&"body"));
        if matches!(script, Script::MissingTls) {
            assert!(!log.contains(&"auth"));
        }
    }
}
#[tokio::test]
async fn only_known_safe_failures_retry_and_uncertain_submissions_never_do() {
    for script in [Script::BeforeTemporary, Script::BodyTemporary] {
        assert!(matches!(
            attempt(script, 465, Ok(())).await.0,
            SmtpOutcome::RetrySafe(_)
        ));
    }
    for script in [Script::LostAck, Script::WrongFinal] {
        assert!(matches!(
            attempt(script, 465, Ok(())).await.0,
            SmtpOutcome::Uncertain(_)
        ));
    }
    assert_eq!(
        attempt(Script::QuitFailed, 465, Ok(())).await.0,
        SmtpOutcome::Accepted
    );
}
#[tokio::test(start_paused = true)]
async fn thirty_second_deadline_distinguishes_prebody_from_uncertain_body() {
    assert!(matches!(
        attempt(Script::HangBefore, 465, Ok(())).await.0,
        SmtpOutcome::RetrySafe(_)
    ));
    assert!(matches!(
        attempt(Script::HangBody, 465, Ok(())).await.0,
        SmtpOutcome::Uncertain(_)
    ));
}
#[test]
fn credentials_and_fixed_errors_never_debug_addresses_passwords_or_provider_text() {
    let credential = SmtpCredential::new(
        "saved@example.invalid".into(),
        SecretString::new("test-not-real-app-password".into()).unwrap(),
    )
    .unwrap();
    let debug = format!("{credential:?} {:?}", SmtpFailure::Permanent);
    assert!(!debug.contains("saved@"));
    assert!(!debug.contains("test-not-real"));
}

#[tokio::test(start_paused = true)]
async fn dns_and_smtp_share_one_thirty_second_attempt_deadline() {
    struct SlowResolver;
    impl SmtpResolver for SlowResolver {
        async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, DestinationError> {
            tokio::time::sleep(Duration::from_secs(4)).await;
            Ok(vec!["8.8.8.8".parse().unwrap()])
        }
    }
    let started = tokio::time::Instant::now();
    let destination = resolve_smtp_destination("smtp.example.invalid", 465, &SlowResolver)
        .await
        .unwrap();
    let sender = EmailSender::new(
        "Synthetic".into(),
        "sender@example.invalid".into(),
        "saved@example.invalid".into(),
    )
    .unwrap();
    let message = render_saved_account_test(
        &support::event(),
        "Synthetic",
        "https://company.example.invalid",
        &sender,
    )
    .unwrap();
    let credential = SmtpCredential::new(
        "saved@example.invalid".into(),
        SecretString::new("test-only".into()).unwrap(),
    )
    .unwrap();
    let log = Arc::new(Mutex::new(vec![]));
    let connector = Connector {
        script: Script::HangBefore,
        log: log.clone(),
    };
    assert!(matches!(
        send_email_attempt_with_connector(
            &connector,
            &destination,
            SmtpTlsMode::Implicit,
            &credential,
            &message,
            || async { Ok(()) }
        )
        .await,
        SmtpOutcome::RetrySafe(_)
    ));
    assert_eq!(
        tokio::time::Instant::now() - started,
        Duration::from_secs(30)
    );
    log.lock().unwrap().clear();
    assert!(matches!(
        send_email_attempt_with_connector(
            &connector,
            &destination,
            SmtpTlsMode::Implicit,
            &credential,
            &message,
            || async { Ok(()) }
        )
        .await,
        SmtpOutcome::RetrySafe(_)
    ));
    assert!(
        log.lock().unwrap().is_empty(),
        "an expired destination never opens a new connection"
    );
}
