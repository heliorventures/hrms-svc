// Actual command/transaction rehearsals complement these delegation alarms.
#[test]
fn email_bridge_follows_durable_replay_and_precedes_command_receipt() {
    let source = include_str!("../src/service.rs");
    let replay = source
        .find("if let Some(result) = recorded_command")
        .unwrap();
    let bridge = source
        .find("emit_loan_email")
        .expect("missing loan transaction producer");
    assert!(bridge > replay && bridge < source.find("INSERT INTO loan_command_receipt").unwrap());
}
#[test]
fn save_and_posting_commands_cannot_create_approval_email() {
    let source = include_str!("../src/email_emit.rs");
    assert!(source.contains("LoanCommand::SubmitRequest"));
    assert!(source.contains("LoanCommand::DecideRequest"));
    assert!(!source.contains("LoanCommand::SaveRequest"));
    assert!(!source.contains("LoanCommand::RecordDisbursement"));
}
