use super::keyring::SecretString;
use async_graphql::{InputObject, SimpleObject};
use kabipay_common::{KabiPayError, KabiPayResult};
use serde::{Deserialize, Serialize};
use uuid::Uuid;

#[derive(
    Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize, SimpleObject, InputObject,
)]
#[graphql(input_name = "EmailCategoriesInput")]
#[serde(deny_unknown_fields)]
pub struct EmailCategories {
    pub leave: bool,
    pub approval: bool,
    pub payslip: bool,
    pub missed_punch_out: bool,
}
impl EmailCategories {
    pub(crate) fn entries(self) -> [(&'static str, bool); 4] {
        [
            ("leave", self.leave),
            ("approval", self.approval),
            ("payslip", self.payslip),
            ("missed_punch_out", self.missed_punch_out),
        ]
    }
}

pub enum SecretChange {
    Preserve,
    Replace(SecretString),
    Clear,
}
impl SecretChange {
    pub const fn action(&self) -> &'static str {
        match self {
            Self::Preserve => "preserve",
            Self::Replace(_) => "replace",
            Self::Clear => "clear",
        }
    }
}
impl std::fmt::Debug for SecretChange {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_tuple("SecretChange")
            .field(&self.action())
            .finish()
    }
}

#[derive(Clone, Serialize, SimpleObject)]
pub struct EmailSettingsPublic {
    pub id: Uuid,
    pub version: i64,
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: i32,
    pub tls_mode: String,
    pub sender_name: String,
    pub sender_email: String,
    pub account_email: String,
    pub categories: EmailCategories,
    pub credential_present: bool,
}
impl std::fmt::Debug for EmailSettingsPublic {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("EmailSettingsPublic")
            .field("id", &self.id)
            .field("version", &self.version)
            .field("enabled", &self.enabled)
            .field("credential_present", &self.credential_present)
            .finish_non_exhaustive()
    }
}

pub struct EmailSettingsInput {
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: i32,
    pub sender_name: String,
    pub sender_email: String,
    pub account_email: String,
    pub categories: EmailCategories,
    pub credential: SecretChange,
}

pub(crate) fn invalid_configuration() -> KabiPayError {
    KabiPayError::BusinessRule {
        code: "EMAIL_SETTINGS_INVALID",
        message: "Review the email server, port and sender settings.".into(),
    }
}
pub(crate) fn valid_mailbox(value: &str) -> bool {
    if value.len() > 254
        || !value.is_ascii()
        || value
            .bytes()
            .any(|byte| byte.is_ascii_whitespace() || byte.is_ascii_control())
    {
        return false;
    }
    let Some((local, domain)) = value.split_once('@') else {
        return false;
    };
    !local.is_empty()
        && local.len() <= 64
        && !local.starts_with('.')
        && !local.ends_with('.')
        && !local.contains("..")
        && local
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b".!#$%&'*+-/=?^_`{|}~".contains(&byte))
        && valid_hostname(domain)
}
pub(crate) fn valid_hostname(value: &str) -> bool {
    super::destination::smtp_hostname_is_valid(value)
}

impl EmailSettingsInput {
    pub fn validate(&self) -> KabiPayResult<()> {
        if !matches!(self.smtp_port, 465 | 587)
            || self.sender_name.chars().count() > 120
            || self.sender_name.chars().any(char::is_control)
        {
            return Err(invalid_configuration());
        }
        let blank = !self.enabled
            && self.smtp_host.is_empty()
            && self.sender_email.is_empty()
            && self.account_email.is_empty()
            && !matches!(self.credential, SecretChange::Replace(_));
        if !blank
            && (!valid_hostname(&self.smtp_host)
                || !valid_mailbox(&self.sender_email)
                || !valid_mailbox(&self.account_email))
        {
            return Err(invalid_configuration());
        }
        Ok(())
    }
    pub(crate) fn normalized(mut self) -> KabiPayResult<Self> {
        self.smtp_host = self
            .smtp_host
            .trim()
            .trim_end_matches('.')
            .to_ascii_lowercase();
        self.sender_name = self.sender_name.trim().into();
        self.sender_email = self.sender_email.trim().into();
        self.account_email = self.account_email.trim().into();
        if matches!(self.credential, SecretChange::Clear) {
            self.enabled = false;
        }
        self.validate()?;
        Ok(self)
    }
    pub(crate) fn transport_changed(&self, previous: &EmailSettingsPublic) -> bool {
        self.smtp_host != previous.smtp_host
            || self.smtp_port != previous.smtp_port
            || self.account_email != previous.account_email
    }
    pub fn validate_credential_change(&self, previous: &EmailSettingsPublic) -> KabiPayResult<()> {
        if previous.credential_present
            && self.transport_changed(previous)
            && matches!(self.credential, SecretChange::Preserve)
        {
            return Err(KabiPayError::BusinessRule {
                code: "EMAIL_TRANSPORT_REQUIRES_CREDENTIAL_CHANGE",
                message:
                    "Replace or clear the credential when changing the email server or account."
                        .into(),
            });
        }
        Ok(())
    }
}
