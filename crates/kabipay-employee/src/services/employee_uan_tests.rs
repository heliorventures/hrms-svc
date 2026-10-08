use super::employee_uan_service::normalize_uan;

#[test]
fn uan_keeps_leading_zeroes_and_trims_outer_whitespace() {
    assert_eq!(
        normalize_uan(" 012345678901 ").unwrap(),
        Some("012345678901".into())
    );
}

#[test]
fn blank_uan_explicitly_clears_the_optional_value() {
    assert_eq!(normalize_uan("   ").unwrap(), None);
}

#[test]
fn uan_rejects_non_ascii_digits_and_incorrect_lengths() {
    for value in [
        "12345678901",
        "1234567890123",
        "12345678901A",
        "123456 789012",
        "１２３４５６７８９０１２",
    ] {
        assert!(normalize_uan(value).is_err());
    }
}
