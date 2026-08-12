//! Auto-generated from `kabipay-database/changelog/migrations/0007_employee_core/employee_core.xml`.

pub mod employee {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub user_id: Option<Uuid>,
        pub department_id: Option<Uuid>,
        pub designation_id: Option<Uuid>,
        pub cost_center_id: Option<Uuid>,
        pub location_id: Option<Uuid>,
        pub reporting_manager_id: Option<Uuid>,
        pub employee_code: String,
        pub first_name: String,
        pub last_name: String,
        pub date_of_birth: Option<NaiveDate>,
        pub gender: Option<String>,
        pub blood_group: Option<String>,
        pub nationality: Option<String>,
        pub employment_type: Option<String>,
        pub status: String,
        pub date_of_joining: NaiveDate,
        pub probation_end_date: Option<NaiveDate>,
        pub notice_period_days: Option<i32>,
        pub emergency_contact_name: Option<String>,
        pub emergency_contact_phone: Option<String>,
        pub emergency_contact_relation: Option<String>,
        pub personal_phone: Option<String>,
        pub current_address: Option<String>,
        pub permanent_address: Option<String>,
        pub uan_number: Option<String>,
        pub esic_number: Option<String>,
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

pub mod employment_history {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employment_history")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub department_id: Option<Uuid>,
        pub designation_id: Option<Uuid>,
        pub cost_center_id: Option<Uuid>,
        pub salary: Option<Decimal>,
        pub effective_from: NaiveDate,
        pub effective_to: Option<NaiveDate>,
        pub change_reason: Option<String>,
        pub changed_by: Option<Uuid>,
        pub is_deleted: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_pan {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_pan")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub pan_number: String,
        pub is_primary: bool,
        pub is_verified: bool,
        pub verified_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_aadhaar {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_aadhaar")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub aadhaar_last4: String,
        pub is_primary: bool,
        pub is_verified: bool,
        pub verified_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_bank {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_bank")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub account_number: String,
        pub ifsc_code: String,
        pub bank_name: String,
        pub account_type: Option<String>,
        pub is_primary: bool,
        pub is_verified: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod dependent {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "dependent")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub name: String,
        pub relationship: Option<String>,
        pub date_of_birth: Option<NaiveDate>,
        pub gender: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
