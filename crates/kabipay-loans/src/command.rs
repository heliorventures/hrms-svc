use crate::LoanResult;
use serde::Serialize;
use sha2::{Digest, Sha256};
pub fn command_hash<T: Serialize>(input: &T) -> LoanResult<String> {
    // Canonicalize recursively even if another crate enables serde_json/preserve_order.
    fn canonical(value: serde_json::Value) -> serde_json::Value {
        match value {
            serde_json::Value::Object(values) => serde_json::Value::Object(
                values
                    .into_iter()
                    .map(|(key, value)| (key, canonical(value)))
                    .collect::<std::collections::BTreeMap<_, _>>()
                    .into_iter()
                    .collect(),
            ),
            serde_json::Value::Array(values) => {
                serde_json::Value::Array(values.into_iter().map(canonical).collect())
            }
            other => other,
        }
    }
    let bytes = serde_json::to_vec(&canonical(serde_json::to_value(input)?))?;
    Ok(hex::encode(Sha256::digest(bytes)))
}
