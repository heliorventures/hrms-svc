use kabipay_notification::email::rotation::{RotationMode, RotationRequest};
use uuid::Uuid;
#[tokio::test]
async fn relative_secret_files_are_refused_without_reading_them() {
    assert!(
        kabipay_notification::email::keyring::Keyring::from_file(std::path::Path::new(
            "relative-secret.json"
        ))
        .await
        .is_err()
    );
}

#[test]
fn rotation_requires_exact_database_schema_tenant_version_and_distinct_keys() {
    let request = RotationRequest {
        tenant_id: Uuid::new_v4(),
        expected_database: "company_example".into(),
        expected_schema: "tenant_example".into(),
        expected_version: 3,
        current_key_id: "v1".into(),
        new_key_id: "v2".into(),
        mode: RotationMode::Preview,
    };
    assert!(request.validate().is_ok());
    let mut invalid = request.clone();
    invalid.tenant_id = Uuid::nil();
    assert!(invalid.validate().is_err());
    for database in [
        "postgres",
        "company_a; DROP DATABASE postgres",
        "",
        "company_../x",
    ] {
        invalid = request.clone();
        invalid.expected_database = database.into();
        assert!(invalid.validate().is_err());
    }
    for schema in [
        "public",
        "pg_catalog",
        "tenant_x,public",
        "",
        "tenant_a;SELECT 1",
    ] {
        invalid = request.clone();
        invalid.expected_schema = schema.into();
        assert!(invalid.validate().is_err());
    }
    invalid = request.clone();
    invalid.expected_version = 0;
    assert!(invalid.validate().is_err());
    invalid = request.clone();
    invalid.new_key_id = invalid.current_key_id.clone();
    assert!(invalid.validate().is_err());
    invalid = request;
    invalid.new_key_id = "bad\nkey".into();
    assert!(invalid.validate().is_err());
}
