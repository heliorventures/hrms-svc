//! Auto-generated from `hrms-database/changelog/migrations/0084_survey_targeting_corrections/survey_targeting_corrections.xml`.

pub mod survey_audience_location {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "survey_audience_location")]
    pub struct Model {
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub survey_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub location_id: Uuid,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod survey_audience_employee {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "survey_audience_employee")]
    pub struct Model {
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub survey_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub employee_id: Uuid,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod survey_revision {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "survey_revision")]
    pub struct Model {
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub survey_id: Uuid,
        pub source_survey_id: Uuid,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod survey_audience_scope {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "survey_audience_scope")]
    pub struct Model {
        pub tenant_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub survey_id: Uuid,
        pub audience_kind: String,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
