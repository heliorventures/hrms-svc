//! SeaORM model for migration 0060 durable private-file cleanup tombstones,
//! hardened with explicit claim ownership by migration 0062.

pub mod private_file_cleanup_task {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "private_file_cleanup_task")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub file_storage_id: Option<Uuid>,
        pub deduplication_key: String,
        pub provider: String,
        pub bucket: Option<String>,
        pub storage_path: Option<String>,
        pub local_root: Option<String>,
        pub status: String,
        pub attempt_count: i32,
        pub next_attempt_at: DateTimeUtc,
        pub claimed_at: Option<DateTimeUtc>,
        pub claim_token: Option<Uuid>,
        pub last_error_class: Option<String>,
        pub completed_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
