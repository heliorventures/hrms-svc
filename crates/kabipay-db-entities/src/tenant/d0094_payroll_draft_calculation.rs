pub mod payroll_draft_calculation {
    use crate::tenant::prelude::*;
    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "payroll_draft_calculation")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub cycle_id: Uuid,
        pub revision: i32,
        pub fingerprint: String,
        pub snapshot: Json,
        pub calculated_by: Uuid,
        pub calculated_at: DateTimeUtc,
        pub finalized_at: Option<DateTimeUtc>,
        pub finalized_by: Option<Uuid>,
        pub acknowledgement: Option<Json>,
    }
    impl ActiveModelBehavior for ActiveModel {}
    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
