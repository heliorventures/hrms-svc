//! Auto-generated from `hrms-database/changelog/migrations/0097_location_working_calendar/location_working_calendar.xml`.

pub mod working_calendar_profile {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "working_calendar_profile")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        pub activation_date: NaiveDate,
        pub revision: i64,
        pub activated_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_location_assignment {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_location_assignment")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub location_id: Option<Uuid>,
        pub effective_from: NaiveDate,
        pub revision: i64,
        pub changed_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod weekly_off_policy_version {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "weekly_off_policy_version")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub location_id: Option<Uuid>,
        pub effective_from: NaiveDate,
        pub inherits_default: bool,
        pub fixed_weekdays: Json,
        pub saturday_ordinals: Json,
        pub created_by: Uuid,
        pub created_at: DateTimeUtc,
        pub superseded_at: Option<DateTimeUtc>,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
