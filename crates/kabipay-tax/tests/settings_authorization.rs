use kabipay_tax::services::tax_settings::check_revision;
#[test]
fn only_the_reviewed_revision_can_be_superseded() {
    assert!(check_revision(None, None).is_ok());
    assert!(check_revision(Some(1), Some(1)).is_ok());
    assert!(check_revision(Some(1), None).is_err());
    assert!(check_revision(Some(2), Some(1)).is_err());
    assert!(check_revision(None, Some(1)).is_err());
}
