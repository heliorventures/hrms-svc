//! Auto-generated from `hrms-database/changelog/migrations/0063_attendance_management/attendance_management.xml`.

pub mod attendance_adjustment_audit {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "attendance_adjustment_audit")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub attendance_id: Uuid,
        pub target_employee_id: Uuid,
        pub actor_user_id: Uuid,
        pub operation: String,
        pub reason: String,
        pub before_values: Option<Json>,
        pub after_values: Json,
        pub request_id: Option<String>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
