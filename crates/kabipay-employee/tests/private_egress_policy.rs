#[path = "../src/services/private_egress_policy.rs"]
mod policy;

#[test]
fn rejects_nonpublic_destination_addresses_and_requires_exact_host_allowance() {
    for address in ["0.0.0.0", "10.0.0.1", "100.64.0.1", "127.0.0.1", "169.254.169.254", "172.16.0.1", "192.168.0.1", "192.0.2.1", "198.19.0.1", "198.51.100.1", "203.0.113.1", "224.0.0.1", "255.255.255.255", "::", "::1", "::ffff:127.0.0.1", "fc00::1", "fe80::1", "ff02::1", "2001:db8::1", "2002:7f00:1::"] {
        assert!(!policy::public_destination(address.parse().unwrap()), "allowed {address}");
    }
    for address in ["1.1.1.1", "8.8.8.8", "2606:4700:4700::1111"] {
        assert!(policy::public_destination(address.parse().unwrap()), "rejected public {address}");
    }
    assert!(policy::allowed_host("hooks.example.com", &["hooks.example.com".into()]));
    for host in ["hooks.example.com.attacker.com", "example.com", "localhost", "hooks.example.com.", "127.0.0.1"] {
        assert!(!policy::allowed_host(host, &["hooks.example.com".into()]));
    }
}
