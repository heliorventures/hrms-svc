//! Auto-generated from `kabipay-database/changelog/migrations/0005_auth_rbac/auth_rbac.xml`.

pub mod user {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "user")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub username: String,
        pub email: Option<String>,
        pub password_hash: String,
        pub must_change_password: bool,
        pub is_active: bool,
        pub mfa_enabled: bool,
        pub mfa_secret: Option<String>,
        pub last_login_at: Option<DateTimeUtc>,
        pub is_deleted: bool,
        pub deleted_at: Option<DateTimeUtc>,
        pub deleted_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod role {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "role")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub name: String,
        pub description: Option<String>,
        pub is_system_role: bool,
        pub is_deleted: bool,
        pub deleted_at: Option<DateTimeUtc>,
        pub deleted_by: Option<Uuid>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod permission {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "permission")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub resource: String,
        pub action: String,
        pub module_id: Uuid,
        pub description: Option<String>,
        pub created_at: DateTimeUtc,
        pub updated_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod role_permission {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "role_permission")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub role_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub permission_id: Uuid,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod user_role {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "user_role")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub user_id: Uuid,
        #[sea_orm(primary_key, auto_increment = false)]
        pub role_id: Uuid,
        pub assigned_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod user_session {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "user_session")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub user_id: Uuid,
        pub token_hash: String,
        pub ip_address: Option<String>,
        pub user_agent: Option<String>,
        pub created_at: DateTimeUtc,
        pub expires_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}

pub mod permission_scope {
    use crate::tenant::prelude::*;

    #[derive(Clone, Debug, PartialEq, DeriveEntityModel)]
    #[sea_orm(table_name = "permission_scope")]
    pub struct Model {
        #[sea_orm(primary_key, auto_increment = false)]
        pub id: Uuid,
        pub tenant_id: Uuid,
        pub role_id: Uuid,
        pub resource: String,
        pub action: String,
        pub scope_type: String,
        pub created_at: DateTimeUtc,
    }

    impl ActiveModelBehavior for ActiveModel {}

    #[derive(Copy, Clone, Debug, EnumIter, DeriveRelation)]
    pub enum Relation {}
}
