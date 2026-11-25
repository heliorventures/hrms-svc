//! Typed durable important-alert contract. SMTP remains private to Notification.
mod transport_contract;
mod types;
mod writer;
pub use transport_contract::*;
mod eligibility;
pub use eligibility::*;
pub use types::*;
pub use writer::{append_email_intent, AppendIntentResult};

mod organization_identity;
pub use organization_identity::*;
mod workflow;
pub use workflow::*;
mod reviewers;
pub use reviewers::*;
mod emission;
pub use emission::*;
