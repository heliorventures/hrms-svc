use kabipay_common::{KabiPayError, KabiPayResult};

const ALLOWED_ROOTS: &[&str] = &[
    "attendance",
    "expenses",
    "leave",
    "notifications",
    "profile",
    "timesheet",
    "hr",
    "admin",
    "organization",
    "payroll",
    "workplace",
];

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NotificationAction(String);

impl NotificationAction {
    pub fn parse_internal_route(raw: &str) -> KabiPayResult<Option<Self>> {
        let value = raw.trim();
        if value.is_empty() {
            return Ok(None);
        }
        if !value.starts_with('/')
            || value.starts_with("//")
            || value.contains('\\')
            || value.chars().any(char::is_control)
        {
            return Err(invalid_action_url());
        }

        let decoded = percent_decode(value).ok_or_else(invalid_action_url)?;
        if decoded.contains('\\')
            || decoded.chars().any(char::is_control)
            || decoded.split(['?', '#']).next().is_some_and(|path| {
                path.split('/').any(|segment| segment == "." || segment == "..")
            })
        {
            return Err(invalid_action_url());
        }

        let suffix_index = value.find(['?', '#']).unwrap_or(value.len());
        let (path, suffix) = value.split_at(suffix_index);
        let normalized_path = format!(
            "/{}",
            path.split('/').filter(|segment| !segment.is_empty()).collect::<Vec<_>>().join("/")
        );
        let root = normalized_path
            .trim_start_matches('/')
            .split('/')
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        if !ALLOWED_ROOTS.contains(&root.as_str()) {
            return Err(invalid_action_url());
        }
        Ok(Some(Self(format!("{normalized_path}{suffix}"))))
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

fn invalid_action_url() -> KabiPayError {
    KabiPayError::Validation(
        "action URL must be an allowed internal application path".into(),
    )
}

fn percent_decode(value: &str) -> Option<String> {
    let bytes = value.as_bytes();
    let mut decoded = Vec::with_capacity(bytes.len());
    let mut index = 0;
    while index < bytes.len() {
        if bytes[index] == b'%' {
            if index + 2 >= bytes.len() {
                return None;
            }
            let high = hex_value(bytes[index + 1])?;
            let low = hex_value(bytes[index + 2])?;
            decoded.push((high << 4) | low);
            index += 3;
        } else {
            decoded.push(bytes[index]);
            index += 1;
        }
    }
    String::from_utf8(decoded).ok()
}

fn hex_value(value: u8) -> Option<u8> {
    match value {
        b'0'..=b'9' => Some(value - b'0'),
        b'a'..=b'f' => Some(value - b'a' + 10),
        b'A'..=b'F' => Some(value - b'A' + 10),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn accepts_and_normalizes_allowed_internal_routes() {
        let action = NotificationAction::parse_internal_route(" /expenses//claims?id=123 ")
            .unwrap()
            .unwrap();
        assert_eq!(action.into_string(), "/expenses/claims?id=123");
    }

    #[test]
    fn rejects_external_traversal_and_unknown_routes() {
        for value in [
            "https://evil.example/path",
            "//evil.example/path",
            "javascript:alert(1)",
            "/expenses/../admin",
            "/expenses/%2e%2e/admin",
            "/unknown/path",
            "/\\evil.example/path",
        ] {
            assert!(NotificationAction::parse_internal_route(value).is_err(), "{value}");
        }
    }

    #[test]
    fn empty_action_is_stored_as_null() {
        assert_eq!(NotificationAction::parse_internal_route("  ").unwrap(), None);
    }
}
