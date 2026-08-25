//! Auto-generated from `kabipay-database/changelog/migrations/0017_onboarding_offboarding/onboarding_offboarding.xml`.

pub mod onboarding_checklist {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "onboarding_checklist")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub task_name: String,
        pub task_category: Option<String>,
        pub assigned_to: Option<Uuid>,
        pub is_completed: bool,
        pub due_date: Option<NaiveDate>,
        pub completed_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod separation {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "separation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub separation_type: String,
        pub resignation_date: Option<NaiveDate>,
        pub last_working_date: NaiveDate,
        pub reason: Option<String>,
        pub status: String,
        pub approved_by: Option<Uuid>,
        pub workflow_instance_id: Option<Uuid>,
        pub offboarded_at: Option<DateTimeUtc>,
        pub offboarding_event_id: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod exit_interview {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "exit_interview")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub separation_id: Uuid,
        pub conducted_by: Option<Uuid>,
        pub reason_for_leaving: Option<String>,
        pub feedback: Option<String>,
        pub satisfaction_score: Option<i32>,
        pub conducted_at: DateTimeUtc,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod fnf_settlement {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "fnf_settlement")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub separation_id: Uuid,
        pub leave_encashment: Option<Decimal>,
        pub gratuity_amount: Option<Decimal>,
        pub bonus_payable: Option<Decimal>,
        pub recovery_amount: Option<Decimal>,
        pub net_payable: Option<Decimal>,
        pub status: String,
        pub processed_at: Option<DateTimeUtc>,
        pub processed_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod clearance_checklist {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "clearance_checklist")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub separation_id: Uuid,
        pub department: String,
        pub task_name: String,
        pub is_cleared: bool,
        pub cleared_by: Option<Uuid>,
        pub cleared_at: Option<DateTimeUtc>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
