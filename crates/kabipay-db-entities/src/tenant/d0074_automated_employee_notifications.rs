//! Auto-generated from `hrms-database/changelog/migrations/0074_automated_employee_notifications/automated_employee_notifications.xml`.

pub mod notification_automation_setting {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "notification_automation_setting")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub birthday_enabled: bool,
        pub work_anniversary_enabled: bool,
        pub company_sharing_enabled: bool,
        pub delivery_local_time: NaiveTime,
        pub birthday_title_template: String,
        pub birthday_message_template: String,
        pub anniversary_title_template: String,
        pub anniversary_message_template: String,
        pub updated_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod employee_celebration_preference {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "employee_celebration_preference")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub employee_id: Uuid,
        pub share_birthday: bool,
        pub share_work_anniversary: bool,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod automated_notification_occurrence {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "automated_notification_occurrence")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub event_type: String,
        pub employee_id: Uuid,
        pub event_date: NaiveDate,
        pub recipient_user_id: Uuid,
        pub notification_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
