//! Actual service schemas validate UI documents offline; absent claims stop before database access.
use async_graphql::{EmptySubscription, Request, Schema, Variables};

fn documents(source: &str) -> Vec<&str> {
    source
        .split("/* GraphQL */")
        .skip(1)
        .map(|part| part.split_once('`').unwrap().1.split_once('`').unwrap().0)
        .collect()
}
fn variables() -> Variables {
    Variables::from_json(
        serde_json::json!({"employeeId":"10000000-0000-0000-0000-000000000003",
        "componentId":"10000000-0000-0000-0000-000000000004","payslipId":"10000000-0000-0000-0000-000000000005",
        "year":2026,"month":9,"asOf":null,"visible":false,"expectedRevision":null,"input":{}}),
    )
}

#[tokio::test]
async fn payroll_documents_match_the_actual_service_contract_and_require_authority() {
    let schema = Schema::build(
        kabipay_payroll::resolvers::QueryRoot,
        kabipay_payroll::resolvers::MutationRoot,
        EmptySubscription,
    )
    .finish();
    for source in [
        include_str!("../../../../hrms-ui/src/modules/payroll/periodInputTypes.ts"),
        include_str!("../../../../hrms-ui/src/modules/payroll/payslipPresentation.ts"),
        include_str!("../../../../hrms-ui/src/modules/payroll/importSalaryPreview.ts"),
        include_str!(
            "../../../../hrms-ui/src/modules/payroll/components/CompanyPayslipComponents.tsx"
        ),
        include_str!("../../../../hrms-ui/src/modules/payroll/components/ApprovedLwpReview.tsx"),
    ] {
        for document in documents(source) {
            let response = schema
                .execute(Request::new(document).variables(variables()))
                .await;
            assert!(
                !response.errors.is_empty(),
                "private payroll reads must require claims"
            );
            assert!(
                response.errors.iter().all(|error| !error.path.is_empty()),
                "GraphQL document validation failed: {:?}",
                response.errors
            );
        }
    }
}

#[tokio::test]
async fn leave_history_document_matches_the_actual_service_contract_and_requires_authority() {
    let schema = Schema::build(
        kabipay_leave::resolvers::QueryRoot,
        kabipay_leave::resolvers::MutationRoot,
        EmptySubscription,
    )
    .finish();
    for document in documents(include_str!(
        "../../../../hrms-ui/src/modules/leave/importedLeaveTypes.ts"
    )) {
        let response = schema
            .execute(Request::new(document).variables(variables()))
            .await;
        assert!(!response.errors.is_empty());
        assert!(
            response.errors.iter().all(|error| !error.path.is_empty()),
            "GraphQL document validation failed: {:?}",
            response.errors
        );
    }
}

#[tokio::test]
async fn imported_profile_document_requires_existing_employee_authority() {
    let schema = Schema::build(
        kabipay_employee::resolvers::QueryRoot,
        kabipay_employee::resolvers::MutationRoot,
        EmptySubscription,
    )
    .finish();
    for document in documents(include_str!(
        "../../../../hrms-ui/src/modules/organization/employee-profile/importedProfile.ts"
    )) {
        let response = schema
            .execute(Request::new(document).variables(variables()))
            .await;
        assert!(!response.errors.is_empty());
        assert!(
            response.errors.iter().all(|error| !error.path.is_empty()),
            "GraphQL document validation failed: {:?}",
            response.errors
        );
    }
}
