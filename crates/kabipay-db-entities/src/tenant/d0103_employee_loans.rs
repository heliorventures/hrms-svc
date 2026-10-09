//! Auto-generated from `hrms-database/changelog/migrations/0103_employee_loans/employee_loans.xml`.

pub mod loan_policy_version {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_policy_version")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub policy_key: String,
        pub version: i32,
        pub status: String,
        pub currency: String,
        pub minor_units: i32,
        pub effective_from: NaiveDate,
        pub effective_to: Option<NaiveDate>,
        pub rules: Json,
        pub calculator_version: String,
        pub approved_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_request {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_request")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub requested_amount: Decimal,
        pub currency: String,
        pub purpose: String,
        pub employee_notes: Option<String>,
        pub management_notes: Option<String>,
        pub preferences: Json,
        pub state: String,
        pub workflow_instance_id: Option<Uuid>,
        pub version: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_account {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_account")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub request_id: Uuid,
        pub loan_number: String,
        pub approved_principal: Decimal,
        pub currency: String,
        pub minor_units: i32,
        pub state: String,
        pub funding_state: String,
        pub current_terms_id: Option<Uuid>,
        pub accrued_through: Option<NaiveDate>,
        pub rounding_carry: Decimal,
        pub version: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_terms_version {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_terms_version")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub policy_version_id: Uuid,
        pub version: i64,
        pub effective_from: NaiveDate,
        pub terms: Json,
        pub approved_by: Uuid,
        pub agreement_evidence: Json,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_schedule_version {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_schedule_version")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub terms_version_id: Uuid,
        pub version: i64,
        pub effective_from: NaiveDate,
        pub monthly_amount: Decimal,
        pub recovery_mode: String,
        pub reason: String,
        pub actor_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_schedule_item {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_schedule_item")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub schedule_version_id: Uuid,
        pub due_date: NaiveDate,
        pub principal: Decimal,
        pub interest: Decimal,
        pub total: Decimal,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_period_override {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_period_override")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub period_start: NaiveDate,
        pub action: String,
        pub amount: Option<Decimal>,
        pub accrual_treatment: String,
        pub reason: String,
        pub actor_id: Uuid,
        pub version: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_disbursement {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_disbursement")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub amount: Decimal,
        pub value_date: NaiveDate,
        pub method: String,
        pub external_reference: String,
        pub evidence: Json,
        pub actor_id: Uuid,
        pub version: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_receipt {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_receipt")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub amount: Decimal,
        pub value_date: NaiveDate,
        pub method: String,
        pub external_reference: String,
        pub evidence: Json,
        pub actor_id: Uuid,
        pub version: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_posting {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_posting")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub kind: String,
        pub source_kind: String,
        pub source_id: Uuid,
        pub source_revision: i64,
        pub value_date: NaiveDate,
        pub amount: Decimal,
        pub principal_delta: Decimal,
        pub interest_delta: Decimal,
        pub terms_version_id: Uuid,
        pub reversal_of: Option<Uuid>,
        pub calculation_evidence: Json,
        pub actor_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_ledger_entry {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_ledger_entry")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub posting_id: Uuid,
        pub sequence: i64,
        pub principal_delta: Decimal,
        pub interest_delta: Decimal,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_allocation {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_allocation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub posting_id: Uuid,
        pub principal: Decimal,
        pub interest: Decimal,
        pub direction: String,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_unapplied_credit {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_unapplied_credit")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Uuid,
        pub receipt_id: Uuid,
        pub amount: Decimal,
        pub remaining_amount: Decimal,
        pub version: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_employee_state {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_employee_state")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub revision: i64,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_command_receipt {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_command_receipt")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub actor_id: Uuid,
        pub idempotency_key: String,
        pub command: String,
        pub payload_hash: String,
        pub result: Json,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_audit_event {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_audit_event")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub loan_id: Option<Uuid>,
        pub employee_id: Option<Uuid>,
        pub actor_id: Uuid,
        pub action: String,
        pub before_version: Option<i64>,
        pub after_version: Option<i64>,
        pub reason: Option<String>,
        pub references: Json,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_internal_outbox {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_internal_outbox")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub event_type: String,
        pub aggregate_revision: i64,
        pub payload: Json,
        pub available_at: DateTimeUtc,
        pub attempts: i32,
        pub delivered_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod loan_consumer_receipt {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "loan_consumer_receipt")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub event_id: Uuid,
        pub consumer: String,
        pub processed_at: DateTimeUtc,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
