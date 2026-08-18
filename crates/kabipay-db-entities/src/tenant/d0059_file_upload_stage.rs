//! Auto-generated from `kabipay-database/changelog/migrations/0059_file_upload_stage/file_upload_stage.xml`.

pub mod file_upload_stage {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "file_upload_stage")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub file_storage_id: Uuid,
        pub purpose: String,
        pub created_by: Uuid,
        pub expires_at: DateTimeUtc,
        pub claimed_at: Option<DateTimeUtc>,
        pub claimed_resource_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
