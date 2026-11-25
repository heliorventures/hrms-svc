use super::{
    keyring::{configured_keyring, SecretString},
    settings::{self, EmailCategories, EmailSettingsInput, EmailSettingsPublic, SecretChange},
    test_queue::{self, EmailTestStatus},
};
use async_graphql::{Context, Enum, InputObject, Result};
use kabipay_common::{
    context::{
        ClientClaims, ScopeType, PERM_EMAIL_SETTINGS_MANAGE, PERM_EMAIL_SETTINGS_READ,
        PERM_EMAIL_SETTINGS_TEST,
    },
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError, KabiPayResult,
};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub enum EmailSettingsAction {
    Read,
    Manage,
    Test,
}
pub fn require_email_authority(
    claims: &ClientClaims,
    action: EmailSettingsAction,
) -> KabiPayResult<()> {
    let permission = match action {
        EmailSettingsAction::Read => PERM_EMAIL_SETTINGS_READ,
        EmailSettingsAction::Manage => PERM_EMAIL_SETTINGS_MANAGE,
        EmailSettingsAction::Test => PERM_EMAIL_SETTINGS_TEST,
    };
    let allowed = |code: &str| {
        claims.has_any_permission(&[code])
            && claims.scope_for_permission(code) == Some(ScopeType::All)
    };
    if !allowed(PERM_EMAIL_SETTINGS_READ) || !allowed(permission) {
        return Err(KabiPayError::Forbidden(
            "company email settings require the exact ALL-scoped permissions".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Enum)]
pub enum EmailCredentialAction {
    Preserve,
    Replace,
    Clear,
}
#[derive(InputObject)]
pub struct SaveEmailSettingsInput {
    pub expected_version: i64,
    pub enabled: bool,
    pub smtp_host: String,
    pub smtp_port: i32,
    pub sender_name: String,
    pub sender_email: String,
    pub account_email: String,
    pub categories: EmailCategories,
    pub credential_action: EmailCredentialAction,
    #[graphql(secret)]
    pub credential_value: Option<String>,
}
impl std::fmt::Debug for SaveEmailSettingsInput {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SaveEmailSettingsInput([redacted])")
    }
}
impl SaveEmailSettingsInput {
    fn into_service(self) -> KabiPayResult<EmailSettingsInput> {
        let credential = match (self.credential_action, self.credential_value) {
            (EmailCredentialAction::Preserve, None) => SecretChange::Preserve,
            (EmailCredentialAction::Clear, None) => SecretChange::Clear,
            (EmailCredentialAction::Replace, Some(secret)) => {
                SecretChange::Replace(SecretString::new(secret)?)
            }
            _ => {
                return Err(KabiPayError::Validation(
                    "choose an explicit valid email credential action".into(),
                ))
            }
        };
        EmailSettingsInput {
            enabled: self.enabled,
            smtp_host: self.smtp_host,
            smtp_port: self.smtp_port,
            sender_name: self.sender_name,
            sender_email: self.sender_email,
            account_email: self.account_email,
            categories: self.categories,
            credential,
        }
        .normalized()
    }
}

pub async fn read(ctx: &Context<'_>) -> Result<EmailSettingsPublic> {
    let claims = require_client_claims(ctx)?;
    require_email_authority(claims, EmailSettingsAction::Read)
        .map_err(KabiPayError::into_graphql)?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    settings::load_email_settings(&db, tenant)
        .await
        .map_err(KabiPayError::into_graphql)
}
pub async fn save(ctx: &Context<'_>, input: SaveEmailSettingsInput) -> Result<EmailSettingsPublic> {
    let claims = require_client_claims(ctx)?;
    require_email_authority(claims, EmailSettingsAction::Manage)
        .map_err(KabiPayError::into_graphql)?;
    let tenant = require_tenant_id(ctx)?;
    let expected = input.expected_version;
    let input = input.into_service().map_err(KabiPayError::into_graphql)?;
    let db = tenant_db(ctx, tenant).await?;
    let keys = if input.enabled || matches!(input.credential, SecretChange::Replace(_)) {
        Some(
            configured_keyring()
                .await
                .map_err(KabiPayError::into_graphql)?,
        )
    } else {
        None
    };
    settings::save_email_settings(&db, tenant, claims.sub, expected, input, keys.as_ref())
        .await
        .map_err(KabiPayError::into_graphql)
}
pub async fn queue_test(ctx: &Context<'_>, expected_version: i64) -> Result<EmailTestStatus> {
    let claims = require_client_claims(ctx)?;
    require_email_authority(claims, EmailSettingsAction::Test)
        .map_err(KabiPayError::into_graphql)?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    test_queue::queue_test(&db, tenant, claims.sub, expected_version)
        .await
        .map_err(KabiPayError::into_graphql)
}
pub async fn test_status(ctx: &Context<'_>, id: Uuid) -> Result<EmailTestStatus> {
    let claims = require_client_claims(ctx)?;
    require_email_authority(claims, EmailSettingsAction::Test)
        .map_err(KabiPayError::into_graphql)?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    test_queue::load_test_status(&db, tenant, claims.sub, id)
        .await
        .map_err(KabiPayError::into_graphql)
}
