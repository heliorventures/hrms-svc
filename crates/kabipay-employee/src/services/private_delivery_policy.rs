//! Deterministic source receipt transitions, independent of transport and storage.
#[derive(Debug, PartialEq, Eq)]
pub enum LeaseDecision {
    ReadOnly,
    Claim,
    Reject,
}

pub fn lease_decision(
    status: &str,
    requested: i64,
    current: i64,
    same_owner: bool,
) -> LeaseDecision {
    if requested == 0 || same_owner || !matches!(status, "PENDING" | "RETRYABLE") {
        return LeaseDecision::ReadOnly;
    }
    if current.checked_add(1) == Some(requested) {
        LeaseDecision::Claim
    } else {
        LeaseDecision::Reject
    }
}

pub fn http_outcome(status: u16) -> (&'static str, &'static str) {
    if (200..300).contains(&status) {
        ("DELIVERED", "webhook_http_2xx")
    } else if status == 429 || (500..600).contains(&status) {
        ("RETRYABLE", "webhook_retryable_status")
    } else {
        ("PERMANENT", "webhook_rejected_status")
    }
}
