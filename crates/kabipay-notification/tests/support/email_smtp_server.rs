//! A bounded SMTP simulator on an owned ephemeral loopback socket. It retains
//! command categories only, never AUTH strings, message contents or addresses.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};
use tokio::{
    io::{AsyncBufReadExt, AsyncRead, AsyncWrite, AsyncWriteExt, BufReader},
    net::TcpListener,
    task::JoinHandle,
};
use tokio_rustls::{
    rustls::{
        self,
        pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer},
    },
    TlsAcceptor,
};
#[derive(Clone, Copy)]
pub enum Behavior {
    Accept,
    MissingStartTls,
    AuthenticationRejected,
    TemporaryEnvelope,
    NegativeData,
    TemporaryBody,
    PermanentBody,
    LostAcknowledgement,
    QuitLost,
    FenceRefused,
}
pub struct Server {
    pub address: std::net::SocketAddr,
    pub finished: JoinHandle<Vec<&'static str>>,
}
fn acceptor() -> TlsAcceptor {
    let cert =
        CertificateDer::from_pem_slice(include_bytes!("../fixtures/smtp-test-server.pem")).unwrap();
    let key = PrivateKeyDer::from_pem_slice(include_bytes!("../fixtures/smtp-test-server-key.pem"))
        .unwrap();
    let config = rustls::ServerConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .unwrap()
    .with_no_client_auth()
    .with_single_cert(vec![cert], key)
    .unwrap();
    TlsAcceptor::from(Arc::new(config))
}
async fn line<S: AsyncRead + Unpin>(stream: &mut BufReader<S>) -> String {
    let mut value = String::new();
    let n = stream.read_line(&mut value).await.unwrap_or(0);
    assert!(n < 8192, "bounded synthetic SMTP command");
    value
}
async fn write<S: AsyncWrite + Unpin>(stream: &mut S, value: &str) {
    stream.write_all(value.as_bytes()).await.unwrap();
    stream.flush().await.unwrap();
}
pub async fn start(starttls: bool, behavior: Behavior, fence: Arc<AtomicBool>) -> Server {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let finished = tokio::spawn(async move {
        let (socket, _) = listener.accept().await.unwrap();
        let mut log = vec![];
        let tls = if starttls {
            let mut plain = BufReader::new(socket);
            write(plain.get_mut(), "220 synthetic\r\n").await;
            assert!(line(&mut plain).await.starts_with("EHLO "));
            log.push("ehlo_plain");
            if matches!(behavior, Behavior::MissingStartTls) {
                write(plain.get_mut(), "250-fixture\r\n250 AUTH PLAIN LOGIN\r\n").await;
                assert!(
                    line(&mut plain).await.is_empty(),
                    "no AUTH without STARTTLS"
                );
                return log;
            }
            write(
                plain.get_mut(),
                "250-fixture\r\n250-STARTTLS\r\n250 AUTH PLAIN LOGIN\r\n",
            )
            .await;
            assert_eq!(line(&mut plain).await, "STARTTLS\r\n");
            log.push("starttls");
            assert!(plain.buffer().is_empty());
            write(plain.get_mut(), "220 ready for TLS\r\n").await;
            match acceptor().accept(plain.into_inner()).await {
                Ok(tls) => tls,
                Err(_) => {
                    log.push("tls_rejected");
                    return log;
                }
            }
        } else {
            match acceptor().accept(socket).await {
                Ok(mut tls) => {
                    write(&mut tls, "220 synthetic\r\n").await;
                    tls
                }
                Err(_) => {
                    log.push("tls_rejected");
                    return log;
                }
            }
        };
        log.push("tls");
        let mut stream = BufReader::new(tls);
        assert!(line(&mut stream).await.starts_with("EHLO "));
        log.push("ehlo_tls");
        write(stream.get_mut(), "250-fixture\r\n250 AUTH PLAIN LOGIN\r\n").await;
        assert!(line(&mut stream).await.starts_with("AUTH PLAIN "));
        log.push("auth");
        if matches!(behavior, Behavior::AuthenticationRejected) {
            write(
                stream.get_mut(),
                "535 private-provider-text test-not-real-app-password saved@example.invalid\r\n",
            )
            .await;
            return log;
        }
        write(stream.get_mut(), "235 authenticated\r\n").await;
        assert!(line(&mut stream).await.starts_with("MAIL FROM:"));
        log.push("mail");
        if matches!(behavior, Behavior::TemporaryEnvelope) {
            write(stream.get_mut(), "451 temporary private-provider-text\r\n").await;
            return log;
        }
        write(stream.get_mut(), "250 sender accepted\r\n").await;
        assert!(line(&mut stream).await.starts_with("RCPT TO:"));
        log.push("rcpt");
        write(stream.get_mut(), "250 recipient accepted\r\n").await;
        assert_eq!(line(&mut stream).await, "DATA\r\n");
        log.push("data");
        if matches!(behavior, Behavior::NegativeData) {
            write(stream.get_mut(), "550 rejected before body\r\n").await;
            return log;
        }
        write(stream.get_mut(), "354 content follows\r\n").await;
        if matches!(behavior, Behavior::FenceRefused) {
            assert!(
                line(&mut stream).await.is_empty(),
                "no body or QUIT after refused fence"
            );
            assert!(!fence.load(Ordering::Acquire));
            return log;
        }
        let mut bytes = 0;
        loop {
            let value = line(&mut stream).await;
            if value.is_empty() {
                return log;
            }
            assert!(
                fence.load(Ordering::Acquire),
                "durable fence before first body byte"
            );
            bytes += value.len();
            assert!(bytes < 65536);
            if value == ".\r\n" {
                break;
            }
        }
        log.push("body");
        match behavior {
            Behavior::LostAcknowledgement => return log,
            Behavior::TemporaryBody => {
                write(stream.get_mut(), "451 negative final reply\r\n").await;
                return log;
            }
            Behavior::PermanentBody => {
                write(stream.get_mut(), "550 negative final reply\r\n").await;
                return log;
            }
            _ => {}
        }
        write(stream.get_mut(), "250 accepted\r\n").await;
        if matches!(behavior, Behavior::QuitLost) {
            return log;
        }
        assert_eq!(line(&mut stream).await, "QUIT\r\n");
        log.push("quit");
        write(stream.get_mut(), "221 closing\r\n").await;
        log
    });
    Server { address, finished }
}
