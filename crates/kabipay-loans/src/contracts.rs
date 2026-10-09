use crate::{LoanModuleError, LoanResult};
use serde::{Deserialize, Serialize};
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CommandMeta {
    pub idempotency_key: String,
    pub expected_version: i64,
}
impl CommandMeta {
    pub fn validate(&self) -> LoanResult<()> {
        if self.idempotency_key.trim().is_empty()
            || self.idempotency_key.len() > 128
            || self.idempotency_key.trim() != self.idempotency_key
            || self.expected_version <= 0
        {
            return Err(LoanModuleError::InvalidCommand);
        }
        Ok(())
    }
}
