use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::prelude::Decimal;

pub fn required_text(value: &str, field: &str) -> KabiPayResult<String> {
    let normalized = value.trim();
    if normalized.is_empty() {
        return Err(KabiPayError::Validation(format!("{field} is required")));
    }
    Ok(normalized.to_string())
}

pub fn category_code(value: &str) -> KabiPayResult<String> {
    Ok(required_text(value, "category code")?.to_ascii_uppercase())
}

pub fn optional_identifier(value: Option<String>) -> Option<String> {
    value.and_then(|raw| {
        let normalized = raw.trim().to_string();
        (!normalized.is_empty()).then_some(normalized)
    })
}

pub fn validate_purchase_value(value: Option<Decimal>) -> KabiPayResult<Option<Decimal>> {
    if value.is_some_and(|amount| amount.is_sign_negative()) {
        return Err(KabiPayError::Validation(
            "purchaseValue cannot be negative".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::prelude::Decimal;

    #[test]
    fn required_text_trims_non_blank_values() {
        assert_eq!(required_text("  Laptop  ", "name").unwrap(), "Laptop");
    }

    #[test]
    fn required_text_rejects_blank_values() {
        assert!(required_text("   ", "name").is_err());
    }

    #[test]
    fn category_code_is_trimmed_and_uppercase() {
        assert_eq!(category_code(" lap-top ").unwrap(), "LAP-TOP");
    }

    #[test]
    fn optional_identifier_converts_blank_to_none() {
        assert_eq!(optional_identifier(Some(" tag-1 ".into())), Some("tag-1".into()));
        assert_eq!(optional_identifier(Some("   ".into())), None);
    }

    #[test]
    fn purchase_value_rejects_negative_amounts() {
        assert!(validate_purchase_value(Some(Decimal::new(-1, 0))).is_err());
        assert!(validate_purchase_value(Some(Decimal::new(0, 0))).is_ok());
    }
}
