use async_graphql::{EmptySubscription, Schema};
use kabipay_payroll::resolvers::{MutationRoot, QueryRoot};

#[tokio::test]
async fn loan_resolvers_reject_wrong_tenant_issuer_and_missing_exact_permission_before_database() {
    let tenant = uuid::Uuid::new_v4();
    let base = serde_json::json!({
        "sub":uuid::Uuid::new_v4(),"iss":kabipay_common::context::CLIENT_JWT_ISSUER,
        "exp":9999999999i64,"iat":0,"tenant_id":tenant,
        "permissions":["payroll:manage"],"permission_scopes":{"payroll:manage":"ALL"},
        "roles":["ADMIN"]
    });
    for case in 0..3 {
        let mut value = base.clone();
        if case != 0 {
            value["permissions"] = serde_json::json!(["loan:read"]);
            value["permission_scopes"] = serde_json::json!({"loan:read":"SELF"});
            if case == 1 {
                value["iss"] = serde_json::json!("wrong-issuer");
            } else {
                value["tenant_id"] = serde_json::json!(uuid::Uuid::nil());
            }
        }
        let claims: kabipay_common::context::ClientClaims = serde_json::from_value(value).unwrap();
        let schema = Schema::build(
            QueryRoot::default(),
            MutationRoot::default(),
            EmptySubscription,
        )
        .data(claims)
        .finish();
        for operation in [
            "{ myLoans { nodes { id } } }",
            "{ loanAccount(id:\"00000000-0000-0000-0000-000000000001\") { id } }",
        ] {
            let response = schema.execute(operation).await;
            assert_eq!(response.errors.len(), 1, "{response:?}");
            assert!(!response.errors[0].message.contains("Unknown field"));
            assert!(
                !response.errors[0].message.contains("TenantDbManager"),
                "authority reached database: {response:?}"
            );
        }
    }
}
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
