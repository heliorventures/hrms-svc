//! Tenant-scoped candidate staging. All lifecycle writes serialize on the candidate row.
use std::collections::{BTreeMap, HashSet};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine};
use chrono::{DateTime, Duration, NaiveDate, Utc};
use hmac::{Hmac, Mac};
use rand::RngCore;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection,
    DatabaseTransaction, EntityTrait, QueryFilter, QueryOrder, QuerySelect,
    Set, TransactionTrait,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use sha2::{Digest, Sha256};
use uuid::Uuid;
use kabipay_common::{KabiPayError as Error, KabiPayResult as Result};
use kabipay_db_entities::tenant::d0080_prejoining::{
    prejoining_candidate as candidate, prejoining_config as config,
    prejoining_document as document, prejoining_event as event,
};
use kabipay_db_entities::tenant::d0008_document_system::{
    document_type, employee_document,
};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use super::employee_service::{self, NewEmployee, NewLoginAccount};
pub const MAX_DOCUMENT_BYTES: usize = 10 * 1024 * 1024;
pub fn field_label(key: &str) -> &str {
    match key {
        "firstName" => "First name",
        "lastName" => "Last name",
        "email" => "Email",
        "dateOfBirth" => "Date of birth",
        "gender" => "Gender",
        "bloodGroup" => "Blood group",
        "nationality" => "Nationality",
        "personalPhone" => "Personal phone",
        "currentAddress" => "Current address",
        "permanentAddress" => "Permanent address",
        "emergencyContactName" => "Emergency contact name",
        "emergencyContactPhone" => "Emergency contact phone",
        "emergencyContactRelation" => "Emergency contact relation",
        _ => key,
    }
}
pub const FIELD_KEYS: &[&str] =
    &["firstName", "lastName", "email", "dateOfBirth", "gender", "bloodGroup",
                "nationality", "personalPhone", "currentAddress",
                "permanentAddress", "emergencyContactName",
                "emergencyContactPhone", "emergencyContactRelation"];
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct Field {
    pub key: String,
    pub label: String,
    pub required: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct Requirement {
    pub id: Uuid,
    pub document_type_id: Uuid,
    pub label: String,
    pub required: bool,
}
#[derive(Clone, Debug, Deserialize, Serialize)]
#[serde(rename_all="camelCase", deny_unknown_fields)]
pub struct Config {
    pub expiry_hours: i64,
    pub fields: Vec<Field>,
    pub documents: Vec<Requirement>,
}
impl Default for Config {
    fn default() -> Self {
        Self {
            expiry_hours: 48,
            fields: [("firstName", "First name"), ("lastName", "Last name"),
                                ("email",
                                    "Email")].into_iter().map(|(key, label)|
                        Field {
                            key: key.into(),
                            label: label.into(),
                            required: true,
                        }).collect(),
            documents: vec![],
        }
    }
}
pub fn validation(message: impl Into<String>) -> Error {
    Error::Validation(message.into())
}
fn conflict(code: &'static str, message: &str) -> Error {
    Error::ConflictRule { code, message: message.into() }
}
pub fn decode_config(value: Value) -> Result<Config> {
    let config: Config =
        serde_json::from_value(value).map_err(|_|
                    validation("invalid pre-joining configuration"))?;
    if !(1..=8760).contains(&config.expiry_hours) ||
            config.documents.len() > 20 {
        return Err(validation("expiry must be 1–8760 hours; at most 20 documents"));
    }
    let mut keys = HashSet::new();
    for field in &config.fields {
        if !FIELD_KEYS.contains(&field.key.as_str()) ||
                        !keys.insert(field.key.as_str()) ||
                    field.label.trim().is_empty() || field.label.len() > 100 {
            return Err(validation("invalid or duplicate field"));
        }
    }
    for key in ["firstName", "lastName", "email"] {
        if !config.fields.iter().any(|field|
                        field.key == key && field.required) {
            return Err(validation("firstName, lastName and email must remain required"));
        }
    }
    let mut ids = HashSet::new();
    let mut types = HashSet::new();
    for doc in &config.documents {
        if !ids.insert(doc.id) || !types.insert(doc.document_type_id) ||
                    doc.label.trim().is_empty() || doc.label.len() > 100 {
            return Err(validation("invalid or duplicate document requirement"));
        }
    }
    Ok(config)
}
pub fn normalize_email(value: &str) -> Result<String> {
    let email = value.trim().to_lowercase();
    if email.len() > 254 || email.contains(char::is_whitespace) ||
                        email.chars().any(char::is_control) ||
                    email.split('@').count() != 2 || email.starts_with('@') ||
            !email.split('@').nth(1).is_some_and(|domain|
                        domain.contains('.') && !domain.ends_with('.')) {
        return Err(validation("valid email required"));
    }
    Ok(email)
}
pub fn validate_answers(config: &Config, value: Value, submit: bool)
    -> Result<Value> {
    let input: BTreeMap<String, String> =
        serde_json::from_value(value).map_err(|_|
                    validation("answers must contain text values"))?;
    let mut answers = BTreeMap::new();
    for (key, value) in input {
        if !config.fields.iter().any(|f| f.key == key) {
            return Err(validation("unsupported answer field"));
        }
        let value = value.trim().to_string();
        let max =
            match key.as_str() {
                "firstName" | "lastName" | "nationality" |
                    "emergencyContactRelation" => 100,
                "gender" | "personalPhone" | "emergencyContactPhone" => 50,
                "bloodGroup" => 20,
                "dateOfBirth" => 10,
                "emergencyContactName" => 255,
                "currentAddress" | "permanentAddress" => 2000,
                _ => 254,
            };
        if value.chars().count() > max ||
                value.chars().any(|ch|
                        ch.is_control() &&
                            !(key.ends_with("Address") && matches!(ch,'\n'|'\r'|'\t')))
            {
            return Err(validation(format!("invalid {key}")));
        }
        if !value.is_empty() && key == "dateOfBirth" {
            let date =
                NaiveDate::parse_from_str(&value,
                            "%Y-%m-%d").map_err(|_|
                            validation("dateOfBirth must be YYYY-MM-DD"))?;
            if date > Utc::now().date_naive() {
                return Err(validation("dateOfBirth cannot be in the future"));
            }
        }
        let value =
            if key == "email" && !value.is_empty() {
                normalize_email(&value)?
            } else { value };
        answers.insert(key, value);
    }
    if submit {
        for field in &config.fields {
            if field.required &&
                    !answers.get(&field.key).is_some_and(|v| !v.is_empty()) {
                return Err(validation(format!("{} is required",field.label)));
            }
        }
    }
    Ok(json!(answers))
}
pub fn check_revision(row: &candidate::Model, revision: i32) -> Result<()> {
    if row.revision != revision {
        return Err(conflict("PREJOINING_STALE_REVISION",
                    "This form changed. Refresh before continuing."));
    }
    Ok(())
}
pub fn check_editable(status: &str) -> Result<()> {
    if !matches!(status,"DRAFT"|"CHANGES_REQUESTED") {
        return Err(conflict("PREJOINING_INVALID_STATE",
                    "This form is not open for editing."));
    }
    Ok(())
}
pub fn check_transition(status: &str, action: &str) -> Result<&'static str> {
    match (status, action) {
        ("SUBMITTED", "APPROVE") => Ok("APPROVED"),
        ("SUBMITTED" | "APPROVED", "REQUEST_CHANGES") =>
            Ok("CHANGES_REQUESTED"),
        ("DRAFT" | "SUBMITTED" | "CHANGES_REQUESTED" | "APPROVED", "CANCEL")
            => Ok("CANCELLED"),
        _ =>
            Err(conflict("PREJOINING_INVALID_STATE",
                    "Action is unavailable in the current state.")),
    }
}
pub fn validate_document(mime: &str, bytes: &[u8]) -> Result<()> {
    if bytes.is_empty() || bytes.len() > MAX_DOCUMENT_BYTES {
        return Err(validation("document must be between 1 byte and 10 MiB"));
    }
    let valid =
        match mime {
            "application/pdf" => bytes.starts_with(b"%PDF-"),
            "image/png" => bytes.starts_with(b"\x89PNG\r\n\x1a\n"),
            "image/jpeg" => bytes.starts_with(&[0xff, 0xd8, 0xff]),
            _ => false,
        };
    if !valid {
        return Err(validation("file signature must match PDF, JPEG or PNG MIME type"));
    }
    Ok(())
}
mod invitation;
mod persistence;
mod conversion;
pub use invitation::*;
pub use persistence::*;
pub use conversion::*;
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn required_identity_and_unknown_privileged_fields_fail_closed() {
        let config = Config::default();
        assert!(validate_answers(&config,json!({
            "firstName":"A", "lastName":"B", "email":"a@b.test",
            "roleIds":"admin"
        }),true).is_err());
        assert!(validate_answers(&config,json!({
            "firstName":"A"
        }),true).is_err());
        assert!(decode_config(json!({
            "expiryHours":48, "fields":[], "documents":[]
        })).is_err());
    }
    #[test]
    fn only_submitted_can_be_approved_and_approval_does_not_join() {
        assert_eq!(check_transition("SUBMITTED","APPROVE").unwrap(),"APPROVED");
        for status in
            ["DRAFT", "APPROVED", "JOINED", "CANCELLED", "CHANGES_REQUESTED"]
            {
            assert!(check_transition(status,"APPROVE").is_err());
        }
        assert!(check_editable("SUBMITTED").is_err());
        assert!(check_editable("CHANGES_REQUESTED").is_ok());
    }
    #[test]
    fn document_mime_must_match_signature_and_size() {
        assert!(validate_document("application/pdf",b"<script>").is_err());
        assert!(validate_document("text/html",b"%PDF-").is_err());
        assert!(validate_document("application/pdf",b"%PDF-1.7").is_ok());
        assert!(validate_document("application/pdf",&vec![0;
        MAX_DOCUMENT_BYTES+1]).is_err());
    }
    #[test]
    fn signed_invitation_cannot_be_retargeted() {
        let key = [42; 32];
        let tenant = Uuid::new_v4();
        let id = Uuid::new_v4();
        let payload =
            format!("{tenant}.{id}.{}",URL_SAFE_NO_PAD.encode([1; 32]));
        let token = format!("{payload}.{}",sign(&payload,&key).unwrap());
        assert_eq!(verify_token_with_key(&token,&key).unwrap(),(tenant,id));
        assert!(verify_token_with_key(&token.replace(&tenant.to_string(),&Uuid::new_v4().to_string()),&key).is_err());
        assert!(verify_token_with_key(&token,&[43; 32]).is_err());
    }
}
