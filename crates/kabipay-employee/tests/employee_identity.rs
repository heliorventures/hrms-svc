use async_graphql::{EmptySubscription, Schema};
use kabipay_employee::resolvers::{MutationRoot, QueryRoot};
use serde_json::json;

#[tokio::test]
async fn employee_identity_contract_exposes_optional_corrections() {
    let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).finish();
    let response = schema
        .execute(
            r#"{ __type(name: "UpdateEmployeeInput") {
                inputFields { name type { kind name } }
            } }"#,
        )
        .await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let data = response.data.into_json().unwrap();
    let fields = data["__type"]["inputFields"].as_array().unwrap();
    for (name, scalar) in [("employeeCode", "String"), ("dateOfJoining", "NaiveDate")] {
        let field = fields.iter().find(|field| field["name"] == name);
        assert_eq!(
            field,
            Some(&json!({"name": name, "type": {"kind": "SCALAR", "name": scalar}})),
            "UpdateEmployeeInput must accept optional {name} corrections"
        );
    }
}
