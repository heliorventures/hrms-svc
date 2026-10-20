#[path = "../src/services/private_delivery_policy.rs"]
mod policy;
use policy::{lease_decision, LeaseDecision};

#[test]
fn acknowledged_and_uncertain_destinations_never_resend_after_ack_loss() {
    for status in ["DELIVERED", "DISABLED", "PERMANENT", "UNCERTAIN", "NO_DESTINATION", "PROCESSING"] {
        assert_eq!(lease_decision(status, 2, 1, false), LeaseDecision::ReadOnly);
    }
}
#[test]
fn concurrent_claims_and_status_queries_cannot_jump_the_receipt_fence() {
    for status in ["PENDING", "RETRYABLE"] {
        assert_eq!(lease_decision(status, 0, 4, false), LeaseDecision::ReadOnly);
        assert_eq!(lease_decision(status, 5, 4, false), LeaseDecision::Claim);
        assert_eq!(lease_decision(status, 5, 4, true), LeaseDecision::ReadOnly);
        for generation in [1, 3, 4, 6, i64::MAX] {
            assert_eq!(lease_decision(status, generation, 4, false), LeaseDecision::Reject);
        }
    }
    assert_eq!(lease_decision("PENDING", 1, i64::MAX, false), LeaseDecision::Reject);
}
#[test]
fn receiver_status_keeps_acknowledgement_distinct_from_safe_retry_and_rejection() {
    for status in [200, 201, 204, 299] { assert_eq!(policy::http_outcome(status).0,"DELIVERED"); }
    for status in [429,500,502,503,599] { assert_eq!(policy::http_outcome(status).0,"RETRYABLE"); }
    for status in [301,302,307,308,400,401,403,404,422] { assert_eq!(policy::http_outcome(status).0,"PERMANENT"); }
}
