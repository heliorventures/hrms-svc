//! Bounded explicit SMTP phases. Nothing writes message bytes until the
//! callback confirms the durable Submitting fence. No opaque send/retry loop.
pub use super::smtp_transport::{LettreConnector, LettreSession};
use super::{
    destination::ValidatedSmtpDestination, keyring::SecretString, settings_types::valid_mailbox,
    templates::RenderedEmail,
};
use lettre::Address;
use std::{
    future::Future,
    sync::atomic::{AtomicU8, Ordering},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpTlsMode {
    Implicit,
    RequiredStartTls,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpFailure {
    Temporary,
    Permanent,
    Disconnected,
    Protocol,
    Tls,
}
impl SmtpFailure {
    /// lettre may wrap rustls certificate failures in an InvalidData I/O source
    /// rather than its top-level TLS kind. Inspect fixed types/kinds only; never
    /// copy a provider response, address or credential into an operational error.
    pub fn from_smtp_error(error: lettre::transport::smtp::Error) -> Self {
        let mut source = std::error::Error::source(&error);
        let mut invalid_data = false;
        while let Some(cause) = source {
            if cause
                .downcast_ref::<std::io::Error>()
                .is_some_and(|io| io.kind() == std::io::ErrorKind::InvalidData)
            {
                invalid_data = true;
            }
            source = cause.source();
        }
        if error.is_tls() || invalid_data {
            Self::Tls
        } else if error.is_transient() {
            Self::Temporary
        } else if error.is_permanent() {
            Self::Permanent
        } else if error.is_response() || error.is_client() {
            Self::Protocol
        } else {
            Self::Disconnected
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FenceError {
    Stale,
    Unavailable,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpErrorCode {
    Unavailable,
    Authentication,
    Rejected,
    Uncertain,
    SettingsChanged,
    DestinationRefused,
}
impl SmtpErrorCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Unavailable => "EMAIL_SMTP_UNAVAILABLE",
            Self::Authentication => "EMAIL_SMTP_AUTHENTICATION",
            Self::Rejected => "EMAIL_SMTP_REJECTED",
            Self::Uncertain => "EMAIL_SUBMISSION_UNCERTAIN",
            Self::SettingsChanged => "EMAIL_SETTINGS_CHANGED",
            Self::DestinationRefused => "EMAIL_DESTINATION_REFUSED",
        }
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SmtpOutcome {
    Accepted,
    RetrySafe(SmtpErrorCode),
    Rejected(SmtpErrorCode),
    NotSubmitted(SmtpErrorCode),
    Uncertain(SmtpErrorCode),
}
pub struct SmtpCredential {
    pub(crate) account: String,
    pub(crate) password: SecretString,
}
impl SmtpCredential {
    pub fn new(account: String, password: SecretString) -> Result<Self, SmtpFailure> {
        if !valid_mailbox(&account) {
            return Err(SmtpFailure::Protocol);
        }
        Ok(Self { account, password })
    }
}
impl std::fmt::Debug for SmtpCredential {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("SmtpCredential([redacted])")
    }
}
pub enum SmtpCommand<'a> {
    StartTls,
    Authenticate(&'a SmtpCredential),
    Mail(&'a Address),
    Recipient(&'a Address),
    Data,
    Body(&'a [u8]),
    Quit,
}
impl SmtpCommand<'_> {
    pub fn label(&self) -> &'static str {
        match self {
            Self::StartTls => "starttls",
            Self::Authenticate(_) => "auth",
            Self::Mail(_) => "mail",
            Self::Recipient(_) => "rcpt",
            Self::Data => "data",
            Self::Body(_) => "body",
            Self::Quit => "quit",
        }
    }
}
pub trait SmtpSession: Send {
    fn is_encrypted(&self) -> bool;
    fn can_starttls(&self) -> bool;
    fn command(
        &mut self,
        command: SmtpCommand<'_>,
    ) -> impl Future<Output = Result<u16, SmtpFailure>> + Send;
}
pub trait SmtpConnector: Send + Sync {
    type Session: SmtpSession;
    fn connect(
        &self,
        destination: &ValidatedSmtpDestination,
        mode: SmtpTlsMode,
    ) -> impl Future<Output = Result<Self::Session, SmtpFailure>> + Send;
}
pub async fn send_email_attempt<F, Fut>(
    destination: &ValidatedSmtpDestination,
    mode: SmtpTlsMode,
    credential: &SmtpCredential,
    message: &RenderedEmail,
    before_body: F,
) -> SmtpOutcome
where
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<(), FenceError>> + Send,
{
    send_email_attempt_with_connector(
        &LettreConnector,
        destination,
        mode,
        credential,
        message,
        before_body,
    )
    .await
}
pub async fn send_email_attempt_with_connector<C, F, Fut>(
    connector: &C,
    destination: &ValidatedSmtpDestination,
    mode: SmtpTlsMode,
    credential: &SmtpCredential,
    message: &RenderedEmail,
    before_body: F,
) -> SmtpOutcome
where
    C: SmtpConnector,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<(), FenceError>> + Send,
{
    if !matches!(
        (destination.socket_addr().port(), mode),
        (465, SmtpTlsMode::Implicit) | (587, SmtpTlsMode::RequiredStartTls)
    ) {
        return SmtpOutcome::Rejected(SmtpErrorCode::DestinationRefused);
    }
    // 0 proves no body command started, 1 may have submitted, 2 confirms the SMTP
    // positive final reply. A timeout cannot turn phase 1/2 into a blind resend.
    if tokio::time::Instant::now() >= destination.deadline() {
        return SmtpOutcome::RetrySafe(SmtpErrorCode::Unavailable);
    }
    let phase = AtomicU8::new(0);
    match tokio::time::timeout_at(
        destination.deadline(),
        attempt(
            connector,
            destination,
            mode,
            credential,
            message,
            before_body,
            &phase,
        ),
    )
    .await
    {
        Ok(outcome) => outcome,
        Err(_) => match phase.load(Ordering::Acquire) {
            0 => SmtpOutcome::RetrySafe(SmtpErrorCode::Unavailable),
            2 => SmtpOutcome::Accepted,
            _ => SmtpOutcome::Uncertain(SmtpErrorCode::Uncertain),
        },
    }
}
async fn attempt<C, F, Fut>(
    connector: &C,
    destination: &ValidatedSmtpDestination,
    mode: SmtpTlsMode,
    credential: &SmtpCredential,
    message: &RenderedEmail,
    before_body: F,
    phase: &AtomicU8,
) -> SmtpOutcome
where
    C: SmtpConnector,
    F: FnOnce() -> Fut + Send,
    Fut: Future<Output = Result<(), FenceError>> + Send,
{
    let mut session = match connector.connect(destination, mode).await {
        Ok(s) => s,
        Err(e) => return before_submission(e, SmtpErrorCode::Unavailable),
    };
    if mode == SmtpTlsMode::RequiredStartTls {
        if !session.can_starttls() {
            return SmtpOutcome::Rejected(SmtpErrorCode::DestinationRefused);
        }
        if let Err(e) = session.command(SmtpCommand::StartTls).await {
            return before_submission(e, SmtpErrorCode::DestinationRefused);
        }
    }
    if !session.is_encrypted() {
        return SmtpOutcome::Rejected(SmtpErrorCode::DestinationRefused);
    }
    match session.command(SmtpCommand::Authenticate(credential)).await {
        Ok(235) => {}
        Ok(_) => return SmtpOutcome::Rejected(SmtpErrorCode::Authentication),
        Err(e) => return before_submission(e, SmtpErrorCode::Authentication),
    }
    for command in [
        SmtpCommand::Mail(message.sender()),
        SmtpCommand::Recipient(message.recipient()),
    ] {
        match session.command(command).await {
            Ok(code) if (200..300).contains(&code) => {}
            Ok(_) => return SmtpOutcome::Rejected(SmtpErrorCode::Rejected),
            Err(e) => return before_submission(e, SmtpErrorCode::Rejected),
        }
    }
    match session.command(SmtpCommand::Data).await {
        Ok(354) => {}
        Ok(_) => return SmtpOutcome::Rejected(SmtpErrorCode::Rejected),
        Err(e) => return before_submission(e, SmtpErrorCode::Rejected),
    }
    match before_body().await {
        Ok(()) => {}
        Err(FenceError::Stale) => return SmtpOutcome::NotSubmitted(SmtpErrorCode::SettingsChanged),
        Err(FenceError::Unavailable) => return SmtpOutcome::RetrySafe(SmtpErrorCode::Unavailable),
    }
    phase.store(1, Ordering::Release);
    let outcome = match session.command(SmtpCommand::Body(message.bytes())).await {
        Ok(code) if (200..300).contains(&code) => {
            phase.store(2, Ordering::Release);
            SmtpOutcome::Accepted
        }
        Err(SmtpFailure::Temporary) => SmtpOutcome::RetrySafe(SmtpErrorCode::Unavailable),
        Err(SmtpFailure::Permanent) => SmtpOutcome::Rejected(SmtpErrorCode::Rejected),
        _ => SmtpOutcome::Uncertain(SmtpErrorCode::Uncertain),
    };
    if outcome == SmtpOutcome::Accepted {
        let _ =
            tokio::time::timeout(Duration::from_secs(1), session.command(SmtpCommand::Quit)).await;
    }
    // In DATA mode a refused fence must close the socket; sending QUIT there
    // would itself be message content. The early returns above deliberately drop.
    outcome
}
fn before_submission(error: SmtpFailure, permanent: SmtpErrorCode) -> SmtpOutcome {
    match error {
        SmtpFailure::Tls => SmtpOutcome::Rejected(SmtpErrorCode::DestinationRefused),
        SmtpFailure::Permanent | SmtpFailure::Protocol => SmtpOutcome::Rejected(permanent),
        SmtpFailure::Temporary | SmtpFailure::Disconnected => {
            SmtpOutcome::RetrySafe(SmtpErrorCode::Unavailable)
        }
    }
}
