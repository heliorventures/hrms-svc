//! Auto-generated from `kabipay-database/changelog/migrations/0011_leave/leave.xml`.

pub mod leave_type {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_type")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub name: String,
        pub code: String,
        pub is_paid: bool,
        pub carry_forward: bool,
        pub max_carry_forward_days: Option<i32>,
        pub sandwich_rule: bool,
        pub half_day_allowed: bool,
        pub requires_document: bool,
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

pub mod leave_policy {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_policy")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub leave_type_id: Uuid,
        pub applicable_to: Option<String>,
        pub annual_entitlement: Option<i32>,
        pub accrual_frequency: Option<String>,
        pub accrual_days: Option<Decimal>,
        pub max_consecutive_days: Option<i32>,
        pub min_notice_days: Option<i32>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod leave_balance {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_balance")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub leave_type_id: Uuid,
        pub year: i32,
        pub entitled_days: Decimal,
        pub used_days: Decimal,
        pub pending_days: Decimal,
        pub carried_forward_days: Decimal,
        pub balance_days: Decimal,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod leave_accrual_log {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_accrual_log")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub leave_type_id: Uuid,
        pub days_credited: Decimal,
        pub reason: Option<String>,
        pub credited_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod leave_request {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_request")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub leave_type_id: Uuid,
        pub from_date: NaiveDate,
        pub to_date: NaiveDate,
        pub days_requested: Decimal,
        pub is_half_day: bool,
        pub half_day_session: Option<String>,
        pub status: String,
        pub reason: Option<String>,
        pub rejection_reason: Option<String>,
        /// Historical link/reference retained for requests created before file upload support.
        pub supporting_document_reference: Option<String>,
        pub supporting_document_file_storage_id: Option<Uuid>,
        pub approved_by: Option<Uuid>,
        pub workflow_instance_id: Option<Uuid>,
        pub applied_at: DateTimeUtc,
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
