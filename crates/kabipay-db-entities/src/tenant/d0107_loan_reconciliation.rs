//! Auto-generated from `hrms-database/changelog/migrations/0107_loan_reconciliation/loan_reconciliation.xml`.

pub mod loan_reconciliation_run {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_reconciliation_run")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub employee_id: Uuid,
        pub account_version: i64,
        pub checked_at: DateTimeUtc,
        pub trigger_kind: String,
        pub initiated_by: Option<Uuid>,
        pub fingerprint: String,
        pub report: Json,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_reconciliation_status {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_reconciliation_status")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub last_run_id: Uuid,
        pub blocked: bool,
        pub checked_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
