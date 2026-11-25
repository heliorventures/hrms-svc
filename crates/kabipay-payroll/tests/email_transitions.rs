// Delegation alarms; real finalized/draft owned transactions are rehearsed separately.
#[test]
fn only_processed_cycle_finalization_creates_payslip_available_events() {
    let source = include_str!("../src/services/payroll_finalize.rs");
    let bridge = source
        .find("emit_finalized_payslip_emails")
        .expect("missing committed finalization email bridge");
    assert!(
        bridge
            > source
                .find("UPDATE payroll_cycle SET status='PROCESSED'")
                .unwrap()
    );
    assert!(
        !include_str!("../src/services/payroll_draft.rs").contains("emit_finalized_payslip_emails")
    );
    assert!(!include_str!("../src/services/imported_payroll.rs")
        .contains("emit_finalized_payslip_emails"));
}
