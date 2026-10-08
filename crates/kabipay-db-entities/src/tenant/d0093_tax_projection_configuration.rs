//! Effective-dated settings, declarations and actual history are separate records.
pub mod employee_tax_settings {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_tax_settings")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub revision: i32,
        pub effective_from: NaiveDate,
        pub effective_until: Option<NaiveDate>,
        pub payload: Json,
        pub actor_id: Uuid,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod company_payroll_rule {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "company_payroll_rule")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub revision: i32,
        pub effective_from: NaiveDate,
        pub effective_until: Option<NaiveDate>,
        pub payload: Json,
        pub reason: String,
        pub actor_id: Uuid,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod employee_tax_declaration {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_tax_declaration")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub employee_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub fiscal_year: i32,
        pub revision: i32,
        pub payload: Json,
        pub approved_deductions: Decimal,
        pub actor_id: Uuid,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
pub mod employee_tax_history {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_tax_history")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub fiscal_year: i32,
        pub source_key: String,
        pub revision: i32,
        pub period_start: NaiveDate,
        pub period_end: NaiveDate,
        pub employer: String,
        pub earnings: Decimal,
        pub tds: Option<Decimal>,
        pub coverage: String,
        pub payload: Json,
        pub actor_id: Uuid,
        pub created_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
