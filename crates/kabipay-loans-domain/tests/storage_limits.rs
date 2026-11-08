use kabipay_loans_domain::Currency;
#[test]
fn money_is_rejected_before_exceeding_the_storage_precision() {
    let currency = Currency {
        code: "INR".into(),
        minor_units: 2,
    };
    assert!(currency
        .validate_amount("9999999999999999.99".parse().unwrap())
        .is_ok());
    assert!(currency
        .validate_amount("10000000000000000".parse().unwrap())
        .is_err());
}
