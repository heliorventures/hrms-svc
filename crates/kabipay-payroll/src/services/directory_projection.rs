//! Payroll directory queries keep identity predicates separate from output fields.
use kabipay_db_entities::tenant::{d0005_auth_rbac::user, d0029_file_storage::file_storage};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QuerySelect, Select};
use uuid::Uuid;

pub(crate) fn active_owners(tenant: Uuid) -> Select<user::Entity> {
    user::Entity::find()
        .select_only()
        .columns([user::Column::Id, user::Column::Username])
        .filter(user::Column::TenantId.eq(tenant))
        .filter(user::Column::IsActive.eq(true))
        .filter(user::Column::IsDeleted.eq(false))
}

pub(crate) fn active_owner_id(tenant: Uuid, id: Uuid) -> Select<user::Entity> {
    active_owners(tenant)
        .select_only()
        .column(user::Column::Id)
        .filter(user::Column::Id.eq(id))
}

pub(crate) fn logo_mime(tenant: Uuid, id: Uuid) -> Select<file_storage::Entity> {
    file_storage::Entity::find_by_id(id)
        .select_only()
        .column(file_storage::Column::MimeType)
        .filter(file_storage::Column::TenantId.eq(tenant))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sea_orm::{DbBackend, QueryTrait};

    fn projection(sql: &str) -> &str {
        sql.split_once(" FROM ").expect("SELECT query").0
    }

    #[test]
    fn follow_up_owner_lookup_reads_only_identifier() {
        let tenant = Uuid::from_u128(1);
        let id = Uuid::from_u128(2);
        let sql = active_owner_id(tenant, id).build(DbBackend::Postgres).to_string();
        assert_eq!(projection(&sql), "SELECT \"user\".\"id\"");
        for predicate in ["\"tenant_id\"", "\"is_active\"", "\"is_deleted\"", &id.to_string(), &tenant.to_string()] {
            assert!(sql.contains(predicate), "{sql}");
        }
    }

    #[test]
    fn follow_up_owner_listing_reads_only_display_fields() {
        let sql = active_owners(Uuid::from_u128(1)).build(DbBackend::Postgres).to_string();
        assert_eq!(projection(&sql), "SELECT \"user\".\"id\", \"user\".\"username\"");
    }

    #[test]
    fn payslip_logo_reads_only_mime_type_with_identity_predicates() {
        let sql = logo_mime(Uuid::from_u128(1), Uuid::from_u128(2)).build(DbBackend::Postgres).to_string();
        assert_eq!(projection(&sql), "SELECT \"file_storage\".\"mime_type\"");
        assert!(sql.contains("\"tenant_id\""));
        assert!(sql.contains("\"id\""));
    }
}
