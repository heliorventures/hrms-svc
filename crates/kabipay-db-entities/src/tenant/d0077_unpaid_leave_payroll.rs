//! Tenant unpaid-leave policy and immutable pay-run calculation snapshots.
pub mod payroll_unpaid_leave_allocation {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payroll_unpaid_leave_allocation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub leave_request_id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub from_date: NaiveDate,
        pub to_date: NaiveDate,
        pub approved_days: Decimal,
        pub date_units: Json,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod payroll_unpaid_leave_policy {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payroll_unpaid_leave_policy")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        pub enabled: bool,
        pub basic_component_code: Option<String>,
        pub day_divisor: Option<Decimal>,
        pub treatment: Option<String>,
        pub updated_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod payslip_unpaid_leave {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payslip_unpaid_leave")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub payslip_id: Uuid,
        pub basic_component_code: String,
        pub basic_amount: Decimal,
        pub day_divisor: Decimal,
        pub unpaid_days: Decimal,
        pub amount: Decimal,
        pub treatment: String,
        pub source_days: Json,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
