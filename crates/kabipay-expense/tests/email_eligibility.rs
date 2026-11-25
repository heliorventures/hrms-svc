#[path = "../../kabipay-common/tests/support/email_eligibility.rs"]
mod support;
use kabipay_common::important_email::*;
use kabipay_expense::email_alerts::check_email_eligibility;
use sea_orm::DatabaseConnection;
#[tokio::test]
async fn another_domain_resource_is_denied_before_database_access() {
    let request = support::request(EmailResourceType::Leave);
    assert_eq!(
        check_email_eligibility(
            &DatabaseConnection::Disconnected,
            &DatabaseConnection::Disconnected,
            &request
        )
        .await
        .unwrap(),
        EmailEligibilityDecision::Forbidden
    );
}
#[tokio::test]
async fn missing_resource_is_obsolete_and_read_is_bound_to_owning_company() {
    let request = support::request(EmailResourceType::Expense);
    let (db, queries) = support::empty_db().await;
    assert_eq!(
        check_email_eligibility(&db, &DatabaseConnection::Disconnected, &request)
            .await
            .unwrap(),
        EmailEligibilityDecision::Obsolete
    );
    let queries = queries.lock().unwrap();
    assert_eq!(queries.len(), 1);
    assert!(queries[0].contains("expense"));
    assert!(queries[0].contains(&request.event.tenant_id.to_string()));
    assert!(queries[0].contains(&request.event.resource_id.to_string()));
    assert!(!queries[0].contains("SELECT *"));
}
