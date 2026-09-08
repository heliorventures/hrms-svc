//! Auto-generated from `kabipay-database/changelog/migrations/0027_communication_audit/communication_audit.xml`.

pub mod announcement {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "announcement")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub created_by: Option<Uuid>,
        pub title: String,
        pub body: Option<String>,
        pub target_audience: Option<String>,
        pub target_department_id: Option<Uuid>,
        pub target_location_id: Option<Uuid>,
        pub publish_at: Option<DateTimeUtc>,
        pub expires_at: Option<DateTimeUtc>,
        pub image_file_storage_id: Option<Uuid>,
        pub document_file_storage_id: Option<Uuid>,
        pub video_file_storage_id: Option<Uuid>,
        pub video_link: Option<String>,
        pub post_source: String,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod notification {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "notification")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub user_id: Uuid,
        pub r#type: Option<String>,
        pub title: Option<String>,
        pub message: Option<String>,
        pub action_url: Option<String>,
        pub is_read: bool,
        pub read_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod audit_log {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "audit_log")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub user_id: Option<Uuid>,
        pub entity_type: String,
        pub entity_id: Option<Uuid>,
        pub action: String,
        pub before_state: Option<Json>,
        pub after_state: Option<Json>,
        pub ip_address: Option<String>,
        pub user_agent: Option<String>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
