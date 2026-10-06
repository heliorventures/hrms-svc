use kabipay_tenant_import::contract::ImportPackage;
use serde_json::Value;

fn example() -> Value {
    serde_json::from_str(include_str!(
        "../../../../hrms-database/import-templates/v1/example.synthetic.json"
    ))
    .unwrap()
}

#[test]
fn standard_template_is_consumed_without_client_profile_logic() {
    let package = ImportPackage::parse(&serde_json::to_vec(&example()).unwrap()).unwrap();
    assert_eq!(package.employees.len(), 1);
    assert_eq!(package.period.month, 9);
}

#[test]
fn explicit_location_is_supported_without_changing_legacy_packages() {
    let mut document = example();
    document["employees"][0]["location"] = serde_json::json!({
        "name": "Pune Office", "effective_from": "2026-10-07"
    });
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_ok());
}

#[test]
fn location_requires_a_nonblank_name_and_an_explicit_date() {
    for value in [
        serde_json::json!({"name":"Pune"}),
        serde_json::json!({"name":"  ","effective_from":"2026-10-07"}),
        serde_json::json!({"name":"Pune","effective_from":"invalid"}),
    ] {
        let mut document = example();
        document["employees"][0]["location"] = value;
        assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
    }
}

#[test]
fn unsupported_version_and_unknown_top_level_fields_fail_closed() {
    for (key, value) in [
        ("version", serde_json::json!(2)),
        ("extra", serde_json::json!(true)),
    ] {
        let mut document = example();
        document[key] = value;
        assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
    }
}

#[test]
fn duplicate_employee_identity_blocks_the_batch() {
    let mut document = example();
    let record = document["employees"][0].clone();
    document["employees"].as_array_mut().unwrap().push(record);
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn missing_employee_code_remains_a_deferred_row_without_fabrication() {
    let mut document = example();
    document["employees"][0]["employee"]["code"] = Value::Null;
    let package = ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).unwrap();
    assert!(package.employees[0].core_ready().is_err());
}

#[test]
fn explicit_clearing_required_identity_is_rejected() {
    let mut document = example();
    document["employees"][0]["clear_fields"] = serde_json::json!(["code"]);
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn foreign_source_hash_is_rejected() {
    let mut document = example();
    document["employees"][0]["source_ref"]["file_hash"] = serde_json::json!("1".repeat(64));
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn leave_quota_and_automatic_generation_cannot_be_smuggled_into_configuration() {
    for (section, field, value) in [
        ("leave", "unpaid_quota", serde_json::json!(3)),
        ("payroll", "auto_generate_payslips", serde_json::json!(true)),
    ] {
        let mut document = example();
        document["configuration"][section][field] = value;
        assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
    }
}

#[test]
fn explicit_clear_and_supplied_value_cannot_conflict() {
    let mut document = example();
    document["employees"][0]["clear_fields"] = serde_json::json!(["uan"]);
    document["employees"][0]["employee"]["uan"] = serde_json::json!("123456789012");
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn joining_date_policy_uses_each_employee_date_without_a_fabricated_global_date() {
    let mut document = example();
    document["salary_effective_policy"] = serde_json::json!("JOINING_DATE");
    document["salary_effective_from"] = Value::Null;
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_ok());
    document["salary_effective_policy"] = serde_json::json!("FIXED_DATE");
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_err());
}

#[test]
fn approved_lwp_review_is_an_explicit_optional_monthly_input() {
    let mut document = example();
    document["employees"][0]["period_input"]["approved_lwp_review_hash"] =
        serde_json::json!("a".repeat(64));
    assert!(ImportPackage::parse(&serde_json::to_vec(&document).unwrap()).is_ok());
}
