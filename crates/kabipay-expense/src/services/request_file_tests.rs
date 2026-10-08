//! Evidence requirements must reject missing files before accessing storage.

use super::{expense_service, travel_request_service};
use kabipay_common::KabiPayError;
use rust_decimal::Decimal;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

fn evidence() -> kabipay_db_entities::tenant::d0029_file_storage::file_storage::Model {
    kabipay_db_entities::tenant::d0029_file_storage::file_storage::Model {
        id: Uuid::new_v4(),
        tenant_id: Uuid::nil(),
        provider: "LOCAL".into(),
        bucket: None,
        storage_path: "tenants/company/users/employee/receipt.pdf".into(),
        original_filename: Some("receipt.pdf".into()),
        mime_type: Some("application/pdf".into()),
        file_size_bytes: Some(1024),
        is_public: false,
        uploaded_by: Some(Uuid::nil()),
        created_at: chrono::Utc::now(),
        updated_at: chrono::Utc::now(),
    }
}

#[test]
fn request_file_requires_private_owned_supported_nonempty_evidence() {
    use super::request_file_service::validate_submission_file;
    let mut file = evidence();
    for mime in ["application/pdf", "image/jpeg", "image/png"] {
        file.mime_type = Some(mime.into());
        file.file_size_bytes = Some(6 * 1024 * 1024);
        assert!(validate_submission_file(&file, Uuid::nil(), Uuid::nil()).is_ok());
    }
    assert!(validate_submission_file(&file, Uuid::new_v4(), Uuid::nil()).is_err());
    assert!(validate_submission_file(&file, Uuid::nil(), Uuid::new_v4()).is_err());
    for size in [None, Some(0), Some(-1), Some(6 * 1024 * 1024 + 1)] {
        file.file_size_bytes = size;
        assert!(validate_submission_file(&file, Uuid::nil(), Uuid::nil()).is_err());
    }
    file.file_size_bytes = Some(1024);
    file.mime_type = Some("text/html".into());
    assert!(validate_submission_file(&file, Uuid::nil(), Uuid::nil()).is_err());
    file.mime_type = Some("application/pdf".into());
    file.is_public = true;
    assert!(validate_submission_file(&file, Uuid::nil(), Uuid::nil()).is_err());
    file.is_public = false;
    file.provider = "DATABASE".into();
    assert!(validate_submission_file(&file, Uuid::nil(), Uuid::nil()).is_err());
}

#[tokio::test]
async fn missing_expense_receipt_is_validation_error_before_database_access() {
    let result = expense_service::submit_expense(
        &DatabaseConnection::Disconnected,
        Uuid::nil(),
        Uuid::nil(),
        Uuid::nil(),
        Uuid::nil(),
        Decimal::ONE,
        "INR",
        "2026-10-06".parse().unwrap(),
        "Client meeting",
        None,
        None,
    )
    .await;
    assert!(
        matches!(result, Err(KabiPayError::Validation(ref message))
        if message.contains("supporting file is required")),
        "{result:?}"
    );
}

#[tokio::test]
async fn missing_travel_evidence_is_validation_error_before_database_access() {
    let result = travel_request_service::submit_travel_request(
        &DatabaseConnection::Disconnected,
        Uuid::nil(),
        Uuid::nil(),
        Some("Pune".into()),
        Some("Mumbai".into()),
        "2026-10-07".parse().unwrap(),
        "2026-10-08".parse().unwrap(),
        "Client meeting",
        Some(Decimal::ONE),
        "INR",
        Uuid::nil(),
        None,
    )
    .await;
    assert!(
        matches!(result, Err(KabiPayError::Validation(ref message))
        if message.contains("supporting file is required")),
        "{result:?}"
    );
}
