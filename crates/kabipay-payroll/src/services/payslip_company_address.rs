//! Validated optional company address used by every payslip output.
use kabipay_common::{KabiPayError, KabiPayResult};

pub fn normalize(value: Option<String>) -> KabiPayResult<Option<String>> {
    let Some(value) = value else {
        return Ok(None);
    };
    let value = value.replace("\r\n", "\n").replace('\r', "\n");
    let value = value.trim();
    if value.is_empty() {
        return Ok(None);
    }
    if value.chars().count() > 1000 {
        return Err(KabiPayError::Validation(
            "company address must be at most 1,000 characters".into(),
        ));
    }
    if value
        .chars()
        .any(|character| character.is_control() && character != '\n' && character != '\t')
    {
        return Err(KabiPayError::Validation(
            "company address contains non-printable characters".into(),
        ));
    }
    Ok(Some(value.to_owned()))
}
