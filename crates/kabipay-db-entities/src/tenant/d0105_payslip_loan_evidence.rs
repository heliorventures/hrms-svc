//! Auto-generated from `hrms-database/changelog/migrations/0105_payslip_loan_evidence/payslip_loan_evidence.xml`.

pub mod payslip_loan_snapshot {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payslip_loan_snapshot")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub payslip_id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub cycle_id: Uuid,
        pub source_revision: i64,
        pub value_date: NaiveDate,
        pub currency: String,
        pub recovery_total: Decimal,
        pub snapshot: Json,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
