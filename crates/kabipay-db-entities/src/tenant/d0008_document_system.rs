//! Auto-generated from `kabipay-database/changelog/migrations/0008_document_system/document_system.xml`.

pub mod document_type {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "document_type")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub name: String,
        pub category: Option<String>,
        pub system_key: Option<String>,
        pub is_required: bool,
        pub expiry_alert_days: Option<i32>,
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

pub mod employee_document {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_document")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub document_type_id: Uuid,
        pub file_storage_id: Option<Uuid>,
        pub status: String,
        pub expiry_date: Option<NaiveDate>,
        pub workflow_instance_id: Option<Uuid>,
        pub uploaded_at: DateTimeUtc,
        pub verified_by: Option<Uuid>,
        pub verified_at: Option<DateTimeUtc>,
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

pub mod document_field_definition {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "document_field_definition")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub document_type_id: Uuid,
        pub field_name: String,
        pub field_type: String,
        pub is_required: bool,
        pub display_order: i32,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_document_field {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_document_field")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub document_id: Uuid,
        pub field_definition_id: Uuid,
        pub field_value: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
