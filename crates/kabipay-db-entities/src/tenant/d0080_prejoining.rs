//! Private candidate staging and immutable review history.
pub mod prejoining_config {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "prejoining_config")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        pub config: Json,
        pub updated_by: Uuid,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}
}
pub mod prejoining_candidate {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "prejoining_candidate")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid,
        pub tenant_id: Uuid,
        pub email: String,
        pub status: String,
        pub revision: i32,
        pub config: Json,
        pub answers: Json,
        pub feedback: Option<String>,
        pub invitation_digest: Option<String>,
        pub expires_at: Option<DateTimeUtc>,
        pub employee_id: Option<Uuid>,
        pub created_by: Uuid,
        pub updated_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}
}
pub mod prejoining_event {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "prejoining_event")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid,
        pub tenant_id: Uuid,
        pub candidate_id: Uuid,
        pub revision: i32,
        pub action: String,
        pub snapshot: Json,
        pub actor_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}
}
pub mod prejoining_document {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "prejoining_document")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)] pub id: Uuid,
        pub tenant_id: Uuid,
        pub candidate_id: Uuid,
        pub requirement_id: Uuid,
        pub document_type_id: Uuid,
        pub filename: String,
        pub mime_type: String,
        pub bytes: Vec<u8>,
        pub is_current: bool,
        pub file_storage_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)] pub enum Relation {}
}
