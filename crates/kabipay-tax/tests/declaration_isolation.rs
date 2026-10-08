use kabipay_tax::services::tax_declarations::validate_declaration_calculated_fields;
use rust_decimal::Decimal;

#[test]
fn omitted_legacy_computed_values_are_accepted_without_authority_to_clear_them() {
    assert!(validate_declaration_calculated_fields(None, None, None).is_ok());
}
#[test]
fn self_service_cannot_supply_even_zero_calculated_values() {
    for supplied in [
        (Some(Decimal::ZERO), None, None),
        (None, Some(Decimal::ZERO), None),
        (None, None, Some(Decimal::ZERO)),
    ] {
        assert!(
            validate_declaration_calculated_fields(supplied.0, supplied.1, supplied.2).is_err()
        );
    }
}
