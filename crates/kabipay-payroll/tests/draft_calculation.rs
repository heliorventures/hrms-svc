use kabipay_payroll::services::payroll_draft::{ensure_draft, next_revision};

#[test]
fn repeated_draft_calculations_require_the_current_revision() {
    assert_eq!(next_revision(None, None).unwrap(), 1);
    assert_eq!(next_revision(Some(1), Some(1)).unwrap(), 2);
    assert!(next_revision(Some(2), Some(1)).is_err());
    assert!(next_revision(Some(1), None).is_err());
}

#[test]
fn every_non_draft_cycle_is_immutable() {
    assert!(ensure_draft("DRAFT").is_ok());
    for state in ["PROCESSED", "LOCKED", "CANCELLED", "GENERATED"] {
        assert!(ensure_draft(state).is_err());
    }
}
