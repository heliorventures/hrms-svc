//! Private worker projection, never an API/GraphQL response.
use super::{
    crypto::EncryptedCredential,
    settings::{public_row, stored_credential, EmailSettingsPublic, PUBLIC_PROJECTION},
    store::{DeliveryAttempt, DeliveryCode, DeliveryCutoffs},
};
use chrono::{DateTime, Utc};
use kabipay_common::{
    important_email::{EmailCategory, EmailEventKind},
    KabiPayResult,
};
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement};
pub(crate) struct DeliveryConfiguration {
    pub settings: EmailSettingsPublic,
    pub credential: Option<EncryptedCredential>,
    cutoffs: DeliveryCutoffs,
}
impl DeliveryConfiguration {
    pub fn admission(&self, attempt: &DeliveryAttempt) -> Result<(), DeliveryCode> {
        if attempt.event.expires_at <= Utc::now() {
            return Err(DeliveryCode::Expired);
        }
        if self.settings.version != attempt.settings_version {
            return Err(DeliveryCode::SettingsChanged);
        }
        if !self.settings.credential_present {
            return Err(DeliveryCode::CredentialUnavailable);
        }
        if attempt.event.kind == EmailEventKind::Test {
            if attempt.event.resource_id != self.settings.id
                || attempt.event.resource_version != self.settings.version.to_string()
            {
                return Err(DeliveryCode::SettingsChanged);
            }
        } else if !self.cutoffs.admits(
            attempt.event.category,
            attempt.event.occurred_at,
            attempt.created_at,
        ) {
            return Err(DeliveryCode::Disabled);
        }
        Ok(())
    }
}
pub(crate) async fn load(
    db: &DatabaseConnection,
    tenant: uuid::Uuid,
) -> KabiPayResult<DeliveryConfiguration> {
    let row=db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,format!("SELECT {PUBLIC_PROJECTION},enabled_after,category_cutoffs,credential_cipher,credential_nonce,credential_key_id FROM tenant_email_settings WHERE tenant_id=$1"),[tenant.into()])).await?.ok_or_else(super::settings::configuration_unavailable)?;
    let settings = public_row(&row)?;
    let enabled: Option<DateTime<Utc>> = row.try_get("", "enabled_after")?;
    let cutoffs: serde_json::Value = row.try_get("", "category_cutoffs")?;
    let categories = settings
        .categories
        .entries()
        .into_iter()
        .filter(|(_, on)| *on)
        .filter_map(|(name, _)| {
            let after = DateTime::parse_from_rfc3339(cutoffs.get(name)?.as_str()?)
                .ok()?
                .with_timezone(&Utc);
            let category = match name {
                "leave" => EmailCategory::Leave,
                "approval" => EmailCategory::Approval,
                "payslip" => EmailCategory::Payslip,
                _ => EmailCategory::MissedPunchOut,
            };
            Some((category, after))
        })
        .collect::<Vec<_>>();
    let cutoffs = if settings.enabled {
        enabled
            .map(|after| DeliveryCutoffs::enabled(after, categories))
            .unwrap_or_else(DeliveryCutoffs::disabled)
    } else {
        DeliveryCutoffs::disabled()
    };
    Ok(DeliveryConfiguration {
        settings,
        credential: stored_credential(&row)?,
        cutoffs,
    })
}
