use kabipay_notification::email::rotation::validate_rotation_url;

#[test]
fn operational_rotation_refuses_downgraded_duplicate_or_retargeted_database_urls() {
    let ca = std::env::temp_dir().join("synthetic-pg-ca.pem");
    let mut url =
        reqwest::Url::parse("postgres://notification:synthetic@db.internal/company_example")
            .unwrap();
    url.query_pairs_mut()
        .append_pair("sslmode", "verify-full")
        .append_pair("sslrootcert", ca.to_str().unwrap());
    assert!(validate_rotation_url(url.as_str(), "company_example").is_ok());
    assert!(validate_rotation_url(url.as_str(), "company_other").is_err());
    for mode in ["disable", "allow", "prefer", "require", "verify-ca"] {
        let mut bad = url.clone();
        bad.set_query(None);
        bad.query_pairs_mut()
            .append_pair("sslmode", mode)
            .append_pair("sslrootcert", ca.to_str().unwrap());
        assert!(validate_rotation_url(bad.as_str(), "company_example").is_err());
    }
    for (key, value) in [
        ("sslmode", "disable"),
        ("host", "elsewhere"),
        ("dbname", "other"),
        ("options", "-csearch_path=public"),
    ] {
        let mut bad = url.clone();
        bad.query_pairs_mut().append_pair(key, value);
        assert!(validate_rotation_url(bad.as_str(), "company_example").is_err());
    }
    assert!(validate_rotation_url(
        "postgres://notification:synthetic@db.internal/company_example",
        "company_example"
    )
    .is_err());
}
