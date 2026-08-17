//! Auto-generated from `hrms-database/changelog/migrations/0056_company_documents/company_documents.xml`.

pub mod company_document {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "company_document")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub category: String,
        pub title: String,
        pub description: Option<String>,
        pub file_storage_id: Uuid,
        pub status: String,
        pub visible_to_employees: bool,
        pub uploaded_by: Option<Uuid>,
        pub is_deleted: bool,
        pub deleted_at: Option<DateTimeUtc>,
        pub deleted_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
