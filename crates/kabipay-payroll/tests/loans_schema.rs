use async_graphql::{EmptySubscription, Schema};
use kabipay_payroll::resolvers::{MutationRoot, QueryRoot};
#[tokio::test]
async fn loans_are_composed_without_exposing_internal_financial_coordination() {
    let schema = Schema::build(
        QueryRoot::default(),
        MutationRoot::default(),
        EmptySubscription,
    )
    .finish();
    let sdl = schema.sdl();
    for field in [
        "myLoans(",
        "loanAccounts(",
        "loanRequestQueue(",
        "loanAccount(",
        "loanLedger(",
        "loanPolicyVersions(",
        "previewLoanReversal(",
        "publishLoanPolicy(",
        "retireLoanPolicy(",
        "saveLoanRequest(",
        "submitLoanRequest(",
        "recordLoanReceipt(",
        "setLoanPeriodOverride(",
    ] {
        assert!(sdl.contains(field), "missing {field}")
    }
    for private in [
        "postPayrollRecoveries",
        "postFnfRecoveries",
        "authorizeExitReview",
        "managementNotes",
        "payloadHash",
    ] {
        assert!(!sdl.contains(private), "exposed {private}")
    }
    let response = schema.execute("{ myLoans { nodes { id } } }").await;
    assert!(
        !response.errors.is_empty(),
        "unauthenticated loan access succeeded"
    );
}
