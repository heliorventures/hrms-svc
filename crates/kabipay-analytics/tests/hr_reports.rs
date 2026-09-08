#[path = "../src/entities/mod.rs"] mod entities;
#[path = "../src/services/mod.rs"] mod services;
#[path = "../src/resolvers/mod.rs"] mod resolvers;
use async_graphql::{Schema,EmptySubscription};
#[tokio::test]
async fn hr_reports_require_authorization_before_database_access() {
 let schema=Schema::build(resolvers::QueryRoot,resolvers::MutationRoot,EmptySubscription).finish();
 let response=schema.execute(r#"{ hrReportRows(kind: ATTENDANCE_PUNCTUALITY, fromDate: "2026-09-01", toDate: "2026-09-08") { columns rows totalRows } }"#).await;
 assert!(!response.errors.is_empty());
 assert!(!response.errors[0].message.contains("Unknown field"),"report must exist and reject authorization: {:?}",response.errors);
}
fn claims(permission:&str,scope:Option<&str>)->kabipay_common::context::ClientClaims {
 serde_json::from_value(serde_json::json!({"sub":uuid::Uuid::nil(),"iss":"test","exp":9999999999i64,"iat":0,"tenant_id":uuid::Uuid::nil(),"permissions":[permission],"permission_scopes":scope.map(|s|serde_json::json!({permission:s})).unwrap_or(serde_json::json!({}))})).unwrap()
}
#[tokio::test]
async fn report_rejects_self_team_missing_scope_and_payroll_manage_before_database() {
 for (kind,permission,scope) in [("ATTENDANCE_PUNCTUALITY","attendance:read",Some("SELF")),("LEAVE_REQUESTS","leave:read",Some("TEAM")),("PAYROLL_REGISTER","payroll:read",None),("PAYROLL_REGISTER","payroll:manage",Some("ALL")),("TIMESHEET_HOURS","timesheet:read",Some("SELF")),("EMPLOYEE_MOVEMENTS","analytics:read",Some("ALL"))] {
 let schema=Schema::build(resolvers::QueryRoot,resolvers::MutationRoot,EmptySubscription).data(claims(permission,scope)).finish();
 let result=schema.execute(format!(r#"{{hrReportRows(kind:{kind},fromDate:"2026-09-01",toDate:"2026-09-08"){{totalRows}}}}"#)).await;
 assert_eq!(result.errors.len(),1);assert!(result.errors[0].message.contains("domain read permission"),"{:?}",result.errors);
 }
}
#[tokio::test]
async fn authorized_inverted_range_is_rejected_before_database() {
 let schema=Schema::build(resolvers::QueryRoot,resolvers::MutationRoot,EmptySubscription).data(claims("leave:read",Some("ALL"))).finish();
 let result=schema.execute(r#"{hrReportCsv(kind:LEAVE_REQUESTS,fromDate:"2026-09-08",toDate:"2026-09-01"){rowCount}}"#).await;
 assert_eq!(result.errors.len(),1);assert!(result.errors[0].message.contains("fromDate must not exceed"),"{:?}",result.errors);
}
#[tokio::test]
async fn analytics_only_returns_unavailable_metrics_without_domain_database_access() {
 let schema=Schema::build(resolvers::QueryRoot,resolvers::MutationRoot,EmptySubscription).data(claims("analytics:read",Some("ALL"))).finish();
 let result=schema.execute(r#"{hrInsights(fromDate:"2026-09-01",toDate:"2026-09-08"){onTimeDays netSalaryGenerated pendingRequests monthlyPayroll { month } includedPendingDomains}}"#).await;
 assert!(result.errors.is_empty(),"{:?}",result.errors);
 assert_eq!(result.data.into_json().unwrap(),serde_json::json!({"hrInsights":{"onTimeDays":null,"netSalaryGenerated":null,"pendingRequests":null,"monthlyPayroll":null,"includedPendingDomains":[]}}));
}
#[tokio::test]
async fn employee_search_is_supported_by_both_report_operations() {
 let schema=Schema::build(resolvers::QueryRoot,resolvers::MutationRoot,EmptySubscription).finish();
 for field in ["hrReportRows", "hrReportCsv"] {
 let result=schema.execute(format!(r#"{{{field}(kind:LEAVE_REQUESTS,fromDate:"2026-09-01",toDate:"2026-09-08",employeeSearch:"Ana"){{__typename}}}}"#)).await;
 assert_eq!(result.errors.len(),1);
 assert!(!result.errors[0].message.contains("Unknown argument"),"{:?}",result.errors);
 }
}
