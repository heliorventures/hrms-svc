use super::payslip_company_address::normalize;
use crate::resolvers::types::UpsertPayrollComplianceSettingInput;
use async_graphql::{value, InputType, MaybeUndefined};

#[test]
fn payslip_company_address_normalizes_pasted_lines_and_blank_values() {
    assert_eq!(
        normalize(Some("  801, Business Court\r\nPune - 411038  ".into())).unwrap(),
        Some("801, Business Court\nPune - 411038".into())
    );
    assert_eq!(normalize(Some(" \n ".into())).unwrap(), None);
    assert_eq!(normalize(None).unwrap(), None);
}

#[test]
fn payslip_company_address_rejects_oversized_or_non_printable_input() {
    assert!(normalize(Some("a".repeat(1001))).is_err());
    assert!(normalize(Some("Pune\0".into())).is_err());
    assert!(normalize(Some("a".repeat(1000))).is_ok());
    assert!(normalize(Some("Office\t801\nPune".into())).is_ok());
}

#[test]
fn payslip_company_address_graphql_input_distinguishes_omission_null_and_value() {
    let omitted = UpsertPayrollComplianceSettingInput::parse(Some(value!({}))).unwrap();
    assert_eq!(omitted.payslip_company_address, MaybeUndefined::Undefined);
    let clear = UpsertPayrollComplianceSettingInput::parse(Some(value!({
        "payslipCompanyAddress": null
    })))
    .unwrap();
    assert_eq!(clear.payslip_company_address, MaybeUndefined::Null);
    let saved = UpsertPayrollComplianceSettingInput::parse(Some(value!({
        "payslipCompanyAddress": "Pune - 411038"
    })))
    .unwrap();
    assert_eq!(
        saved.payslip_company_address,
        MaybeUndefined::Value("Pune - 411038".into())
    );
}
