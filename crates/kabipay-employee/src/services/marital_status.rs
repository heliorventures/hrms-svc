use kabipay_common::{KabiPayError, KabiPayResult};

/// Empty input clears the optional field; omitted input is handled by the profile updater.
pub fn normalize(value: &str) -> KabiPayResult<Option<String>> {
    let value = value.trim().to_uppercase().replace(' ', "_");
    match value.as_str() {
        "" => Ok(None),
        "SINGLE" | "MARRIED" | "DIVORCED" | "WIDOWED" | "SEPARATED" | "PREFER_NOT_TO_SAY" => {
            Ok(Some(value))
        }
        _ => Err(KabiPayError::Validation(
            "unsupported marital status".into(),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn marital_status_normalizes_clears_and_rejects_unknown_values() {
        assert_eq!(normalize(" married ").unwrap(), Some("MARRIED".into()));
        assert_eq!(
            normalize("prefer not to say").unwrap(),
            Some("PREFER_NOT_TO_SAY".into())
        );
        assert_eq!(normalize(" ").unwrap(), None);
        assert!(normalize("unknown").is_err());
    }
}
