//! Shared payment-state vocabulary for expense commands and reporting filters.
use crate::{KabiPayError, KabiPayResult};

pub const PAYMENT_STATUS_NONE: &str = "NONE";
pub const PAYMENT_STATUS_PENDING: &str = "PENDING_PAYMENT";
pub const PAYMENT_STATUS_PAID: &str = "PAID";
pub const PAYMENT_STATUS_FAILED: &str = "FAILED";
pub const PAYMENT_STATUS_ON_HOLD: &str = "ON_HOLD";

pub fn normalize_expense_payment_status_wire(s: &str) -> KabiPayResult<&'static str> {
    match s.trim() {
        "NONE" | "none" => Ok(PAYMENT_STATUS_NONE),
        "PENDING_PAYMENT" | "pending_payment" | "PendingPayment" => Ok(PAYMENT_STATUS_PENDING),
        "PAID" | "paid" => Ok(PAYMENT_STATUS_PAID),
        "FAILED" | "failed" => Ok(PAYMENT_STATUS_FAILED),
        "ON_HOLD" | "on_hold" | "OnHold" => Ok(PAYMENT_STATUS_ON_HOLD),
        _ => Err(KabiPayError::Validation(
            "unknown expense payment status; expected NONE | PENDING_PAYMENT | PAID | FAILED | ON_HOLD"
                .into(),
        )),
    }
}
