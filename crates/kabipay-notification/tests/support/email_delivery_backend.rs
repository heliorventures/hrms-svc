//! Synthetic DNS + SMTP phases; this connector never opens a network socket.
use super::id;
use kabipay_common::important_email::*;
use kabipay_notification::email::{
    destination::{
        resolve_smtp_destination, DestinationError, SmtpResolver, ValidatedSmtpDestination,
    },
    keyring::SecretString,
    smtp::{
        send_email_attempt_with_connector, FenceError, SmtpCommand, SmtpConnector, SmtpCredential,
        SmtpFailure, SmtpOutcome, SmtpSession, SmtpTlsMode,
    },
    store::{mark_submitting, DeliveryAttempt},
    templates::{render_email, EmailSender},
    worker::{DeliveryBackend, PreparationFailure},
};
use sea_orm::DatabaseConnection;
use std::{
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
};
#[derive(Default)]
pub struct FakeBackend {
    pub sent: AtomicUsize,
    pub revoked: bool,
    pub block_body: bool,
    pub block_prepare: bool,
}
struct Resolver;
impl SmtpResolver for Resolver {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, DestinationError> {
        Ok(vec!["8.8.8.8".parse().unwrap()])
    }
}
struct Connector<'a> {
    backend: &'a FakeBackend,
}
struct Session<'a> {
    backend: &'a FakeBackend,
}
impl<'a> SmtpConnector for Connector<'a> {
    type Session = Session<'a>;
    async fn connect(
        &self,
        _: &ValidatedSmtpDestination,
        _: SmtpTlsMode,
    ) -> Result<Self::Session, SmtpFailure> {
        Ok(Session {
            backend: self.backend,
        })
    }
}
impl SmtpSession for Session<'_> {
    fn is_encrypted(&self) -> bool {
        true
    }
    fn can_starttls(&self) -> bool {
        true
    }
    async fn command(&mut self, command: SmtpCommand<'_>) -> Result<u16, SmtpFailure> {
        Ok(match command {
            SmtpCommand::Authenticate(_) => 235,
            SmtpCommand::Data => 354,
            SmtpCommand::Quit => 221,
            SmtpCommand::StartTls => 220,
            SmtpCommand::Body(_) => {
                self.backend.sent.fetch_add(1, Ordering::Relaxed);
                if self.backend.block_body {
                    std::future::pending::<()>().await;
                }
                250
            }
            _ => 250,
        })
    }
}
impl DeliveryBackend for FakeBackend {
    type Prepared = EmailRecipientIdentity;
    async fn prepare(
        &self,
        _: &DatabaseConnection,
        _: &DeliveryAttempt,
    ) -> Result<Self::Prepared, PreparationFailure> {
        if self.block_prepare {
            return std::future::pending().await;
        }
        Ok(EmailRecipientIdentity {
            source_tenant_id: id(1),
            source_user_id: id(5),
        })
    }
    fn identity(prepared: &Self::Prepared) -> EmailRecipientIdentity {
        *prepared
    }
    async fn submit(
        &self,
        db: &DatabaseConnection,
        attempt: &DeliveryAttempt,
        prepared: &Self::Prepared,
    ) -> SmtpOutcome {
        let sender = EmailSender::new(
            "Fixture".into(),
            "sender@example.invalid".into(),
            "sender@example.invalid".into(),
        )
        .unwrap();
        let recipient = PrivateEmailRecipient::new(
            *prepared,
            "recipient@example.invalid".into(),
            "https://company.example.invalid".into(),
        )
        .unwrap();
        let message = render_email(
            &attempt.event,
            "Fixture",
            "https://company.example.invalid",
            &sender,
            &recipient,
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
            &Connector { backend: self },
            &destination,
            SmtpTlsMode::Implicit,
            &credential,
            &message,
            || async {
                if self.revoked {
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
