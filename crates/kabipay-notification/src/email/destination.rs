//! Resolve once, reject the entire answer set if any address is nonpublic,
//! then give the SMTP connector a concrete socket while retaining TLS SNI.
use std::{
    future::Future,
    net::{IpAddr, Ipv4Addr, Ipv6Addr, SocketAddr},
    time::Duration,
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DestinationError {
    InvalidHost,
    InvalidPort,
    NoAddress,
    TooManyAddresses,
    ForbiddenAddress,
    DnsUnavailable,
}
pub trait SmtpResolver: Send + Sync {
    fn resolve(
        &self,
        host: &str,
        port: u16,
    ) -> impl Future<Output = Result<Vec<IpAddr>, DestinationError>> + Send;
}
pub struct SystemSmtpResolver;
impl SmtpResolver for SystemSmtpResolver {
    async fn resolve(&self, host: &str, port: u16) -> Result<Vec<IpAddr>, DestinationError> {
        let addresses = tokio::net::lookup_host((host, port))
            .await
            .map_err(|_| DestinationError::DnsUnavailable)?;
        Ok(addresses.take(17).map(|address| address.ip()).collect())
    }
}
/// Construction is private: connector callers cannot substitute an unchecked
/// hostname/IP after DNS validation. No error contains the hostname or answer.
pub struct ValidatedSmtpDestination {
    hostname: String,
    socket: SocketAddr,
    deadline: tokio::time::Instant,
}
impl ValidatedSmtpDestination {
    pub fn hostname(&self) -> &str {
        &self.hostname
    }
    pub fn socket_addr(&self) -> SocketAddr {
        self.socket
    }
    pub(crate) fn deadline(&self) -> tokio::time::Instant {
        self.deadline
    }
}
impl std::fmt::Debug for ValidatedSmtpDestination {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ValidatedSmtpDestination([redacted])")
    }
}
pub async fn resolve_smtp_destination(
    host: &str,
    port: u16,
    resolver: &impl SmtpResolver,
) -> Result<ValidatedSmtpDestination, DestinationError> {
    if !smtp_hostname_is_valid(host) {
        return Err(DestinationError::InvalidHost);
    }
    if !matches!(port, 465 | 587) {
        return Err(DestinationError::InvalidPort);
    }
    let deadline = tokio::time::Instant::now() + Duration::from_secs(30);
    let answers = tokio::time::timeout(Duration::from_secs(5), resolver.resolve(host, port))
        .await
        .map_err(|_| DestinationError::DnsUnavailable)??;
    if answers.is_empty() {
        return Err(DestinationError::NoAddress);
    }
    if answers.len() > 16 {
        return Err(DestinationError::TooManyAddresses);
    }
    if answers.iter().any(|address| !public_address(*address)) {
        return Err(DestinationError::ForbiddenAddress);
    }
    Ok(ValidatedSmtpDestination {
        hostname: host.to_ascii_lowercase(),
        socket: SocketAddr::new(answers[0], port),
        deadline,
    })
}
pub(crate) fn smtp_hostname_is_valid(host: &str) -> bool {
    host.len() <= 253
        && host.is_ascii()
        && host.contains('.')
        && host.parse::<IpAddr>().is_err()
        && host.split('.').all(|label| {
            !label.is_empty()
                && label.len() <= 63
                && !label.starts_with('-')
                && !label.ends_with('-')
                && label
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        })
}
pub fn public_address(address: IpAddr) -> bool {
    match address {
        IpAddr::V4(ip) => public_v4(ip),
        IpAddr::V6(ip) => public_v6(ip),
    }
}
fn public_v4(ip: Ipv4Addr) -> bool {
    let [a, b, c, _] = ip.octets();
    !matches!(a, 0 | 10 | 127 | 224..=255)
        && !(a == 100 && (64..=127).contains(&b))
        && !(a == 169 && b == 254)
        && !(a == 172 && (16..=31).contains(&b))
        && !(a == 192 && (b == 168 || (b == 0 && matches!(c, 0 | 2)) || (b == 88 && c == 99)))
        && !(a == 198 && ((18..=19).contains(&b) || (b == 51 && c == 100)))
        && !(a == 203 && b == 0 && c == 113)
}
fn public_v6(ip: Ipv6Addr) -> bool {
    let s = ip.segments();
    // Globally routable unicast only. Deny protocol-assignment, transition,
    // documentation and deprecated 6bone space, including all mapped IPv4 forms.
    s[0] & 0xe000 == 0x2000
        && !(s[0] == 0x2001 && (s[1] < 0x0200 || s[1] == 0x0db8))
        && s[0] != 0x2002
        && s[0] != 0x3ffe
        && !(s[0] == 0x3fff && s[1] <= 0x0fff)
}
