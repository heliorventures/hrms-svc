//! Payroll import configuration and immutable statements (migration 0090).
pub mod payroll_period_input {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payroll_period_input")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub year: i32,
        pub month: i32,
        pub input: Json,
        pub ready: bool,
        pub source_ref: Option<Json>,
        pub revision: i32,
        pub updated_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod employee_payroll_rule {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_payroll_rule")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub employee_id: Uuid,
        pub rules: Json,
        #[sea_orm(primary_key, auto_increment = false)]
        pub effective_from: NaiveDate,
        pub updated_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod payslip_statement {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payslip_statement")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub payslip_id: Uuid,
        pub tenant_id: Uuid,
        pub statement: Json,
        pub period_input_id: Option<Uuid>,
        pub period_input_revision: Option<i32>,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod payroll_period_adjustment {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payroll_period_adjustment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub year: i32,
        pub month: i32,
        pub code: String,
        pub amount: Decimal,
        pub reason: Option<String>,
        pub ready: bool,
        pub updated_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
