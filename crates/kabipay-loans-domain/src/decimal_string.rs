//! Strict wire money: JSON numbers are rejected before any financial calculation.
use rust_decimal::Decimal;
use serde::{Deserialize, Deserializer, Serializer};
pub fn serialize<S: Serializer>(value: &Decimal, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&value.normalize().to_string())
}
pub fn deserialize<'de, D: Deserializer<'de>>(deserializer: D) -> Result<Decimal, D::Error> {
    let value = String::deserialize(deserializer)?;
    if value.trim() != value
        || value.is_empty()
        || value.len() > 40
        || value
            .bytes()
            .any(|b| !b.is_ascii_digit() && b != b'.' && b != b'-')
    {
        return Err(serde::de::Error::custom("expected a plain decimal string"));
    }
    Decimal::from_str_exact(&value).map_err(serde::de::Error::custom)
}
pub mod optional {
    use super::*;
    pub fn serialize<S: Serializer>(
        value: &Option<Decimal>,
        serializer: S,
    ) -> Result<S::Ok, S::Error> {
        match value {
            Some(value) => serializer.serialize_some(&value.normalize().to_string()),
            None => serializer.serialize_none(),
        }
    }
    pub fn deserialize<'de, D: Deserializer<'de>>(
        deserializer: D,
    ) -> Result<Option<Decimal>, D::Error> {
        let value = Option::<String>::deserialize(deserializer)?;
        value
            .map(|value| {
                if value.trim() != value
                    || value.is_empty()
                    || value.len() > 40
                    || value
                        .bytes()
                        .any(|b| !b.is_ascii_digit() && b != b'.' && b != b'-')
                {
                    return Err(serde::de::Error::custom("expected a plain decimal string"));
                }
                Decimal::from_str_exact(&value).map_err(serde::de::Error::custom)
            })
            .transpose()
    }
}
