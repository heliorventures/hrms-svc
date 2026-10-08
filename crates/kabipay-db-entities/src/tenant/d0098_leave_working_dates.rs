//! Auto-generated from `hrms-database/changelog/migrations/0098_leave_working_dates/leave_working_dates.xml`.

pub mod leave_working_date_snapshot {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_working_date_snapshot")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub leave_request_id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub from_date: NaiveDate,
        pub to_date: NaiveDate,
        pub requested_days: Decimal,
        pub date_units: Json,
        pub calendar_provenance: Json,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
