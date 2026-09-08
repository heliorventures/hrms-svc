//! Auto-generated from `hrms-database/changelog/migrations/0075_performance_appraisal_lifecycle/performance_appraisal_lifecycle.xml`.

pub mod performance_program {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "performance_program")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub name: String,
        pub description: Option<String>,
        pub cadence: String,
        pub anchor_date: NaiveDate,
        pub status: String,
        pub include_calibration: bool,
        pub include_acknowledgement: bool,
        pub goal_weight_required: Decimal,
        pub rating_min: Decimal,
        pub rating_max: Decimal,
        pub created_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod appraisal_template {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "appraisal_template")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub performance_program_id: Uuid,
        pub version: i32,
        pub name: String,
        pub status: String,
        pub published_at: Option<DateTimeUtc>,
        pub published_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod appraisal_template_section {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "appraisal_template_section")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub appraisal_template_id: Uuid,
        pub title: String,
        pub description: Option<String>,
        pub display_order: i32,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod appraisal_question {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "appraisal_question")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub section_id: Uuid,
        pub parent_question_id: Option<Uuid>,
        pub question_type: String,
        pub prompt: String,
        pub is_required: bool,
        pub answerer: String,
        pub self_rating_enabled: bool,
        pub manager_rating_enabled: bool,
        pub display_order: i32,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod appraisal_question_option {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "appraisal_question_option")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub question_id: Uuid,
        pub label: String,
        pub score: Option<Decimal>,
        pub display_order: i32,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod performance_participant {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "performance_participant")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub review_cycle_id: Uuid,
        pub employee_id: Uuid,
        pub manager_employee_id: Option<Uuid>,
        pub department_id: Option<Uuid>,
        pub designation_id: Option<Uuid>,
        pub work_location_id: Option<Uuid>,
        pub appraisal_template_id: Uuid,
        pub status: String,
        pub is_excluded: bool,
        pub exclusion_reason: Option<String>,
        pub response_revision: i32,
        pub self_submitted_at: Option<DateTimeUtc>,
        pub manager_submitted_at: Option<DateTimeUtc>,
        pub acknowledged_at: Option<DateTimeUtc>,
        pub acknowledgement_comment: Option<String>,
        pub final_rating: Option<Decimal>,
        pub performance_band: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod appraisal_answer {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "appraisal_answer")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub performance_participant_id: Uuid,
        pub question_id: Uuid,
        pub revision: i32,
        pub employee_text_answer: Option<String>,
        pub employee_selected_option_ids: Option<Json>,
        pub self_rating: Option<Decimal>,
        pub manager_text_answer: Option<String>,
        pub manager_selected_option_ids: Option<Json>,
        pub manager_rating: Option<Decimal>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod continuous_feedback {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "continuous_feedback")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub review_cycle_id: Option<Uuid>,
        pub goal_id: Option<Uuid>,
        pub reviewer_employee_id: Option<Uuid>,
        pub reviewee_employee_id: Uuid,
        pub visibility: String,
        pub observation_date: NaiveDate,
        pub comments: String,
        pub created_by_user_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod performance_admin_exception {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "performance_admin_exception")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub performance_program_id: Option<Uuid>,
        pub review_cycle_id: Option<Uuid>,
        pub exception_code: String,
        pub details: String,
        pub resolved_at: Option<DateTimeUtc>,
        pub resolved_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
