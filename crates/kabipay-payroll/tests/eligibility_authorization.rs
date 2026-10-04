use async_graphql::{EmptySubscription, Schema};
use kabipay_payroll::resolvers::{MutationRoot, QueryRoot};

#[tokio::test]
async fn eligibility_and_automatic_preview_require_management_scope_before_database() {
    for permission in ["payroll:read", "payroll:manage"] {
        for scope in ["SELF", "TEAM", "DEPARTMENT"] {
            let claims: kabipay_common::context::ClientClaims = serde_json::from_value(serde_json::json!({
                "sub":uuid::Uuid::nil(),"iss":"test","exp":9999999999i64,"iat":0,
                "tenant_id":uuid::Uuid::nil(),"permissions":[permission],"permission_scopes":{permission:scope}
            })).unwrap();
            let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).data(claims).finish();
            for operation in [
                r#"{employeePayrollEligibility(employeeId:"00000000-0000-0000-0000-000000000001",asOf:"2026-10-01")}"#,
                r#"{payrollPeriodInput(employeeId:"00000000-0000-0000-0000-000000000001",year:2026,month:10)}"#,
                r#"mutation{saveEmployeePayrollEligibility(employeeId:"00000000-0000-0000-0000-000000000001",input:{effective_from:"2026-10-01",reason:"Confirmed eligibility",eligibility:{pf_applicable:true,esi_applicable:false,disability:false,esi_continuation_until:null,average_daily_wage:null}})}"#,
            ] {
                let response = schema.execute(operation).await;
                assert_eq!(response.errors.len(),1,"{response:?}");
                let message=&response.errors[0].message;
                assert!(message.contains("scope") || message.contains("permission"),"{message}");
                assert!(!message.contains("Unknown field"),"{message}");
            }
        }
    }
}
