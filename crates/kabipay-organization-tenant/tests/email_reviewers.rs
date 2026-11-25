use kabipay_common::important_email::EmailResourceType;
use kabipay_organization_tenant::email_reviewers::select_organization_email_reviewers;
use sea_orm::DatabaseConnection;
use uuid::Uuid;

#[tokio::test]
async fn domains_without_organization_business_actions_never_query_foreign_authority() {
    for resource in [
        EmailResourceType::Expense,
        EmailResourceType::Travel,
        EmailResourceType::TaxProof,
        EmailResourceType::LoanRequest,
        EmailResourceType::ProfileChange,
    ] {
        let result = select_organization_email_reviewers(
            &DatabaseConnection::Disconnected,
            &DatabaseConnection::Disconnected,
            Uuid::new_v4(),
            Uuid::new_v4(),
            resource,
            Uuid::new_v4(),
        )
        .await
        .unwrap();
        assert!(result.is_empty());
    }
}
