//! Committed import runs and section provenance; independent of replaced staff IDs.
pub mod tenant_import_run {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tenant_import_run")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub package_hash: String,
        pub configuration_hash: String,
        pub target_hash: String,
        pub mode: String,
        pub actor_id: Uuid,
        pub report: Json,
        pub committed_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod tenant_import_record {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "tenant_import_record")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub run_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub employee_code: String,
        #[sea_orm(primary_key, auto_increment = false)]
        pub section: String,
        pub content_hash: String,
        pub outcome: String,
        pub source_ref: Json,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
