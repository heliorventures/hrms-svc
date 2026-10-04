use kabipay_payroll::services::payroll_draft::{validate_acknowledgement, FinalizeAcknowledgement};
use uuid::Uuid;

#[test]
fn provisional_decisions_require_exact_employee_acknowledgements() {
    let employee = Uuid::new_v4();
    assert!(validate_acknowledgement(&[employee], &FinalizeAcknowledgement::default()).is_err());
    let acknowledgement = FinalizeAcknowledgement {
        provisional_tax_employees: vec![employee],
    };
    assert!(validate_acknowledgement(&[employee], &acknowledgement).is_ok());
    assert!(validate_acknowledgement(&[], &acknowledgement).is_err());
}
