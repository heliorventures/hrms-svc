//! Auto-generated from `hrms-database/changelog/migrations/0106_loan_exit_coordination/loan_exit_coordination.xml`.

pub mod employee_exit_operation {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_exit_operation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub separation_id: Uuid,
        pub employee_id: Uuid,
        pub actor_id: Uuid,
        pub review_id: Uuid,
        pub separation_fingerprint: String,
        pub payload_hash: String,
        pub authorization_id: Option<Uuid>,
        pub state: String,
        pub result: Option<Json>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_finance_outbox {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_finance_outbox")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub operation_id: Uuid,
        pub event_type: String,
        pub payload: Json,
        pub attempts: i32,
        pub available_at: DateTimeUtc,
        pub delivered_at: Option<DateTimeUtc>,
        pub last_error: Option<String>,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_exit_review {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_exit_review")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub separation_id: Uuid,
        pub employee_id: Uuid,
        pub actor_id: Uuid,
        pub separation_fingerprint: String,
        pub last_working_date: NaiveDate,
        pub settlement_date: NaiveDate,
        pub employee_revision: i64,
        pub quote_fingerprint: String,
        pub preview: Json,
        pub decision: Json,
        pub decision_hash: String,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_exit_approval_guard {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_exit_approval_guard")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub separation_id: Uuid,
        pub review_id: Uuid,
        pub operation_id: Uuid,
        pub actor_id: Uuid,
        pub payload_hash: String,
        pub state: String,
        pub expires_at: DateTimeUtc,
        pub confirmed_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod fnf_financial_review {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "fnf_financial_review")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub fnf_id: Uuid,
        pub employee_id: Uuid,
        pub revision: i64,
        pub value_date: NaiveDate,
        pub currency: String,
        pub minor_units: i32,
        pub input_hash: String,
        pub snapshot: Json,
        pub actor_id: Uuid,
        pub finalized_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod fnf_payout_source {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "fnf_payout_source")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub fnf_id: Uuid,
        pub source_kind: String,
        pub source_id: Uuid,
        pub amount: Decimal,
        pub already_paid: Decimal,
        pub other_recovery: Decimal,
        pub eligible_for_loan: bool,
        pub approval_reference: String,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_exit_arrangement {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_exit_arrangement")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub employee_id: Uuid,
        pub separation_id: Uuid,
        pub origin_kind: String,
        pub origin_id: Uuid,
        pub last_working_date: NaiveDate,
        pub settlement_date: NaiveDate,
        pub decision: Json,
        pub decision_hash: String,
        pub actor_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
