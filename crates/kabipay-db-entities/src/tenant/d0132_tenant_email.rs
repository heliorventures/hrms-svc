//! Durable email persistence. Cipher bytes never appear in entity debug output.
#[derive(Clone, PartialEq, Eq, sea_orm::DeriveValueType)]
pub struct SecretBytes(pub Vec<u8>);

impl std::fmt::Debug for SecretBytes {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("[redacted]")
    }
}

pub mod tenant_email_settings {
    use super::SecretBytes;
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tenant_email_settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub singleton: bool,
        pub version: i64,
        pub enabled: bool,
        pub smtp_host: String,
        pub smtp_port: i32,
        pub tls_mode: String,
        pub sender_name: String,
        pub sender_email: String,
        pub account_email: String,
        pub categories: Json,
        pub enabled_after: Option<DateTimeUtc>,
        pub category_cutoffs: Json,
        pub credential_cipher: Option<SecretBytes>,
        pub credential_nonce: Option<SecretBytes>,
        pub credential_key_id: Option<String>,
        pub updated_by: Option<Uuid>,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod important_email_intent {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "important_email_intent")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub event_id: Uuid,
        pub recipient_key: String,
        pub event_hash: String,
        pub event: Json,
        pub producer: String,
        pub resource_type: String,
        pub resource_id: Uuid,
        pub resource_version: String,
        pub category: String,
        pub kind: String,
        pub occurred_at: DateTimeUtc,
        pub expires_at: DateTimeUtc,
        pub state: String,
        pub next_attempt_at: DateTimeUtc,
        pub attempt_count: i32,
        pub lease_token: Option<Uuid>,
        pub lease_until: Option<DateTimeUtc>,
        pub settings_version: Option<i64>,
        pub delivery_source_tenant: Option<Uuid>,
        pub delivery_source_user: Option<Uuid>,
        pub terminal_at: Option<DateTimeUtc>,
        pub last_error_code: Option<String>,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod email_private_request_receipt {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, Eq, DeriveEntityModel)]
    #[sea_orm(table_name = "email_private_request_receipt")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub audience: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub purpose: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub nonce: Uuid,
        pub request_hash: String,
        pub expires_at: DateTimeUtc,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
