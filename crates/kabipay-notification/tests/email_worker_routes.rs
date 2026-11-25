use kabipay_notification::email::delivery_transport::EmailRoutes;
use uuid::Uuid;
#[test]
fn worker_is_disabled_unless_the_runtime_switch_is_exactly_true() {
    use kabipay_notification::email::worker::email_worker_enabled;
    for value in [None, Some("false"), Some("1"), Some("TRUE"), Some("true "), Some("")] {
        assert!(!email_worker_enabled(value));
    }
    assert!(email_worker_enabled(Some("true")));
}
fn config(origin: &str) -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({"version":1,"companies":[{"tenant_id":Uuid::from_u128(1),"recipient_origin":"https://employee.internal","domains":[{"domain":"leave","origin":origin}]}]})).unwrap()
}
#[test]
fn mail_routes_are_exact_allowlisted_https_roots() {
    let routes = EmailRoutes::from_json(&config("https://leave.internal")).unwrap();
    assert_eq!(
        routes
            .eligibility(Uuid::from_u128(1), "leave")
            .unwrap()
            .as_str(),
        "https://leave.internal/internal/email/eligibility"
    );
    assert!(routes.eligibility(Uuid::from_u128(2), "leave").is_err());
    assert!(routes.eligibility(Uuid::from_u128(1), "payroll").is_err());
    for origin in [
        "http://leave.internal",
        "https://leave.internal/path",
        "https://user:password@leave.internal",
        "https://leave.internal?next=x",
        "https://leave.internal/#fragment",
    ] {
        assert!(EmailRoutes::from_json(&config(origin)).is_err());
    }
}
#[test]
fn mail_routes_refuse_duplicates_unknown_domains_and_oversize_files() {
    let mut value: serde_json::Value =
        serde_json::from_slice(&config("https://leave.internal")).unwrap();
    let duplicate = value["companies"][0].clone();
    value["companies"].as_array_mut().unwrap().push(duplicate);
    assert!(EmailRoutes::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    let mut value: serde_json::Value =
        serde_json::from_slice(&config("https://leave.internal")).unwrap();
    value["companies"][0]["domains"][0]["domain"] = "arbitrary-command".into();
    assert!(EmailRoutes::from_json(&serde_json::to_vec(&value).unwrap()).is_err());
    assert!(EmailRoutes::from_json(&vec![b'x'; 1024 * 1024 + 1]).is_err());
}
