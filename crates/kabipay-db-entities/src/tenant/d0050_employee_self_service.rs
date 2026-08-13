//! Employee self-service profile records introduced by tenant migration 0050.

pub mod employee_education {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_education")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub education_level: String,
        pub qualification: String,
        pub field_of_study: Option<String>,
        pub institution: String,
        pub board_university: Option<String>,
        pub start_date: Option<NaiveDate>,
        pub completion_year: i32,
        pub grade_score: Option<String>,
        pub description: Option<String>,
        pub verification_status: String,
        pub reviewed_by: Option<Uuid>,
        pub reviewed_at: Option<DateTimeUtc>,
        pub rejection_reason: Option<String>,
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

pub mod employee_work_experience {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_work_experience")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub company: String,
        pub role_title: String,
        pub employment_type: Option<String>,
        pub location: Option<String>,
        pub start_date: NaiveDate,
        pub end_date: Option<NaiveDate>,
        pub is_current: bool,
        pub description: Option<String>,
        pub verification_status: String,
        pub reviewed_by: Option<Uuid>,
        pub reviewed_at: Option<DateTimeUtc>,
        pub rejection_reason: Option<String>,
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

pub mod employee_profile_change_request {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_profile_change_request")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub requested_by: Uuid,
        pub request_type: String,
        pub requested_payload: Json,
        pub requested_payload_encrypted: Option<Vec<u8>>,
        pub payload_encryption_version: Option<i16>,
        pub status: String,
        pub supporting_document_id: Option<Uuid>,
        pub reviewed_by: Option<Uuid>,
        pub reviewed_at: Option<DateTimeUtc>,
        pub rejection_reason: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_education_document {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_education_document")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_education_id: Uuid,
        pub employee_document_id: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_work_experience_document {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_work_experience_document")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_work_experience_id: Uuid,
        pub employee_document_id: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
