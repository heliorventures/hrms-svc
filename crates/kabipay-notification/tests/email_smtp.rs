use kabipay_notification::email::destination::{
    resolve_smtp_destination, DestinationError, SmtpResolver,
};
use std::{
    net::IpAddr,
    sync::atomic::{AtomicUsize, Ordering},
};
struct Resolver {
    answers: Vec<IpAddr>,
    calls: AtomicUsize,
}
impl SmtpResolver for Resolver {
    async fn resolve(&self, _: &str, _: u16) -> Result<Vec<IpAddr>, DestinationError> {
        self.calls.fetch_add(1, Ordering::SeqCst);
        Ok(self.answers.clone())
    }
}
#[tokio::test]
async fn smtp_destination_rejects_every_nonpublic_answer_including_mixed_and_mapped_ipv6() {
    for address in [
        "0.0.0.0",
        "10.0.0.1",
        "100.64.0.1",
        "127.0.0.1",
        "169.254.169.254",
        "172.16.0.1",
        "192.168.0.1",
        "192.0.2.1",
        "192.88.99.1",
        "198.18.0.1",
        "198.51.100.1",
        "203.0.113.1",
        "224.0.0.1",
        "255.255.255.255",
        "::",
        "::1",
        "::ffff:127.0.0.1",
        "::ffff:8.8.8.8",
        "fc00::1",
        "fe80::1",
        "ff02::1",
        "2001:db8::1",
        "2002:7f00::1",
        "3fff::1",
    ] {
        for mixed in [false, true] {
            let mut answers = vec![address.parse().unwrap()];
            if mixed {
                answers.push("8.8.8.8".parse().unwrap());
            }
            let resolver = Resolver {
                answers,
                calls: AtomicUsize::new(0),
            };
            assert!(
                resolve_smtp_destination("smtp.example.invalid", 465, &resolver)
                    .await
                    .is_err(),
                "{address} mixed={mixed}"
            );
        }
    }
}
#[tokio::test]
async fn smtp_destination_pins_validated_socket_and_keeps_the_tls_hostname() {
    let resolver = Resolver {
        answers: vec![
            "8.8.8.8".parse().unwrap(),
            "2606:4700:4700::1111".parse().unwrap(),
        ],
        calls: AtomicUsize::new(0),
    };
    let destination = resolve_smtp_destination("smtp.example.invalid", 587, &resolver)
        .await
        .unwrap();
    assert_eq!(destination.hostname(), "smtp.example.invalid");
    assert_eq!(destination.socket_addr().to_string(), "8.8.8.8:587");
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 1);
    assert_eq!(destination.socket_addr().to_string(), "8.8.8.8:587");
}
#[tokio::test]
async fn invalid_hosts_and_ports_fail_before_dns_and_response_size_is_bounded() {
    let resolver = Resolver {
        answers: vec!["8.8.8.8".parse().unwrap()],
        calls: AtomicUsize::new(0),
    };
    for host in [
        "localhost",
        "127.0.0.1",
        "[::1]",
        "smtp.example.invalid/path",
        "smtp.example.invalid\r\nAUTH",
        "a..invalid",
        "-mail.example.invalid",
    ] {
        assert!(resolve_smtp_destination(host, 465, &resolver)
            .await
            .is_err());
    }
    assert!(
        resolve_smtp_destination("smtp.example.invalid", 25, &resolver)
            .await
            .is_err()
    );
    assert_eq!(resolver.calls.load(Ordering::SeqCst), 0);
    let resolver = Resolver {
        answers: vec!["8.8.8.8".parse().unwrap(); 17],
        calls: AtomicUsize::new(0),
    };
    assert!(
        resolve_smtp_destination("smtp.example.invalid", 465, &resolver)
            .await
            .is_err()
    );
}
#[tokio::test]
async fn ordinary_public_ranges_are_not_confused_with_adjacent_special_prefixes() {
    for address in ["192.0.1.1", "8.8.8.8", "1.1.1.1", "2001:4860:4860::8888"] {
        let resolver = Resolver {
            answers: vec![address.parse().unwrap()],
            calls: AtomicUsize::new(0),
        };
        assert!(
            resolve_smtp_destination("smtp.example.invalid", 465, &resolver)
                .await
                .is_ok(),
            "{address}"
        );
    }
}
