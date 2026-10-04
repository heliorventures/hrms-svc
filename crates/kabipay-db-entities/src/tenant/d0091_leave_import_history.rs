//! Dated leave snapshots; cumulative LWP is never a payroll period input.
pub mod leave_import_history {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "leave_import_history")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub leave_type_id: Uuid,
        pub year: i32,
        pub as_of: NaiveDate,
        pub opening: Json,
        pub historical_lwp: Decimal,
        pub ready: bool,
        pub source_ref: Option<Json>,
        pub updated_by: Uuid,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
