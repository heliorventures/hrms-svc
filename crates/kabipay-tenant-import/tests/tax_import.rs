use kabipay_tenant_import::contract::ImportPackage;
use serde_json::{json, Value};
fn example() -> Value {
    serde_json::from_str(include_str!(
        "../../../../hrms-database/import-templates/v1/example.synthetic.json"
    ))
    .unwrap()
}
#[test]
fn optional_tax_sections_do_not_change_old_packages() {
    let old = ImportPackage::parse(&serde_json::to_vec(&example()).unwrap()).unwrap();
    assert!(old.employees[0].tax_settings.is_none());
    assert!(old.employees[0].tax_history.is_empty());
}
#[test]
fn invalid_optional_tax_values_can_be_reported_without_rejecting_core() {
    let mut value = example();
    value["employees"][0]["tax_settings"] = json!({"regime":"NEW","method":"PERCENTAGE_OVERRIDE"});
    value["employees"][0]["tax_history"] = json!([]);
    let package = ImportPackage::parse(&serde_json::to_vec(&value).unwrap()).unwrap();
    assert!(package.employees[0].core_ready().is_ok());
    assert!(kabipay_tenant_import::tax_import::settings_valid(
        package.employees[0].tax_settings.as_ref()
    )
    .is_err());
}
