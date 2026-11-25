//! The production connector consumes only the already-validated SocketAddr.
//! TLS uses the original DNS hostname; no SMTP trace/body logging is enabled.
use super::{
    destination::ValidatedSmtpDestination,
    smtp::{SmtpCommand, SmtpConnector, SmtpFailure, SmtpSession, SmtpTlsMode},
};
use lettre::transport::smtp::{
    authentication::{Credentials, Mechanism},
    client::{AsyncSmtpConnection, TlsParameters},
    commands::{Data, Mail, Rcpt},
    extension::ClientId,
};
use std::time::Duration;
pub struct LettreConnector;
pub struct LettreSession {
    connection: AsyncSmtpConnection,
    tls: TlsParameters,
}
impl LettreSession {
    /// Allows an injected connector to supply its verified transport. Production
    /// always uses LettreConnector below; tests add only their isolated test CA.
    pub fn from_connection(connection: AsyncSmtpConnection, tls: TlsParameters) -> Self {
        Self { connection, tls }
    }
}
impl SmtpConnector for LettreConnector {
    type Session = LettreSession;
    async fn connect(
        &self,
        destination: &ValidatedSmtpDestination,
        mode: SmtpTlsMode,
    ) -> Result<LettreSession, SmtpFailure> {
        let tls = TlsParameters::new_rustls(destination.hostname().to_owned()).map_err(classify)?;
        let implicit = if mode == SmtpTlsMode::Implicit {
            Some(tls.clone())
        } else {
            None
        };
        let connection = AsyncSmtpConnection::connect_tokio1(
            destination.socket_addr(),
            Some(Duration::from_secs(30)),
            &ClientId::default(),
            implicit,
            None,
        )
        .await
        .map_err(classify)?;
        Ok(LettreSession { connection, tls })
    }
}
impl SmtpSession for LettreSession {
    fn is_encrypted(&self) -> bool {
        self.connection.is_encrypted()
    }
    fn can_starttls(&self) -> bool {
        self.connection.can_starttls()
    }
    async fn command(&mut self, command: SmtpCommand<'_>) -> Result<u16, SmtpFailure> {
        let response = match command {
            SmtpCommand::StartTls => {
                self.connection
                    .starttls(self.tls.clone(), &ClientId::default())
                    .await
                    .map_err(classify)?;
                return Ok(220);
            }
            SmtpCommand::Authenticate(credential) => {
                self.connection
                    .auth(
                        &[Mechanism::Plain, Mechanism::Login],
                        &Credentials::new(
                            credential.account.clone(),
                            credential.password.expose().to_owned(),
                        ),
                    )
                    .await
            }
            SmtpCommand::Mail(sender) => {
                self.connection
                    .command(Mail::new(Some(sender.clone()), vec![]))
                    .await
            }
            SmtpCommand::Recipient(recipient) => {
                self.connection
                    .command(Rcpt::new(recipient.clone(), vec![]))
                    .await
            }
            SmtpCommand::Data => self.connection.command(Data).await,
            SmtpCommand::Body(bytes) => self.connection.message(bytes).await,
            SmtpCommand::Quit => self.connection.quit().await,
        }
        .map_err(classify)?;
        Ok(response.code().into())
    }
}
fn classify(error: lettre::transport::smtp::Error) -> SmtpFailure {
    SmtpFailure::from_smtp_error(error)
}
