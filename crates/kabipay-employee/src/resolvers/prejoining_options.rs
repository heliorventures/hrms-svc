//! Directory choices for the privileged employee-conversion action.
use async_graphql::{Context, Json, Result};
use kabipay_common::{subgraph::{require_tenant_id, tenant_db}, KabiPayError};
use kabipay_db_entities::tenant::{
    d0005_auth_rbac::role,
    d0006_org_hierarchy::{department, designation},
    d0007_employee_core::employee,
};
use sea_orm::{ColumnTrait, Condition, EntityTrait, QueryFilter, QueryOrder, QuerySelect, Select};
use serde_json::{json, Value};
use uuid::Uuid;

const MANAGER_PAGE_SIZE: u64 = 50;

fn manager_query(tenant: Uuid, search: &str, offset: u64) -> Select<employee::Entity> {
    let mut query = employee::Entity::find()
        .filter(employee::Column::TenantId.eq(tenant))
        .filter(employee::Column::IsDeleted.eq(false))
        .filter(employee::Column::Status.ne("TERMINATED"));
    for term in search.split_whitespace() {
        use sea_orm::sea_query::{Expr, Func};
        let pattern = format!("%{}%", term.to_lowercase());
        query = query.filter(Condition::any()
            .add(Expr::expr(Func::lower(Expr::col(employee::Column::FirstName))).like(&pattern))
            .add(Expr::expr(Func::lower(Expr::col(employee::Column::LastName))).like(&pattern))
            .add(Expr::expr(Func::lower(Expr::col(employee::Column::EmployeeCode))).like(&pattern)));
    }
    query.order_by_asc(employee::Column::EmployeeCode)
        .order_by_asc(employee::Column::Id)
        .offset(offset)
        .limit(MANAGER_PAGE_SIZE + 1)
}

pub async fn options(
    ctx: &Context<'_>,
    manager_search: Option<String>,
    manager_offset: Option<i32>,
) -> Result<Json<Value>> {
    super::prejoining::gate(ctx, "prejoining:review")?;
    super::scope::require_tenant_rbac_admin(ctx)?;
    super::scope::require_exact_all_scope(ctx, "employee:write")
        .or_else(|_| super::scope::require_exact_all_scope(ctx, "employee:manage"))?;
    let search = manager_search.as_deref().unwrap_or_default().trim();
    if search.len() > 100 || manager_offset.is_some_and(|offset| offset < 0) {
        return Err(KabiPayError::Validation("Manager search is limited to 100 characters; offset must be nonnegative.".into()).into_graphql());
    }
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    let departments = department::Entity::find()
        .filter(department::Column::TenantId.eq(tenant))
        .filter(department::Column::IsDeleted.eq(false))
        .order_by_asc(department::Column::Name).order_by_asc(department::Column::Id)
        .all(&db);
    let designations = designation::Entity::find()
        .filter(designation::Column::TenantId.eq(tenant))
        .filter(designation::Column::IsDeleted.eq(false))
        .order_by_asc(designation::Column::Title).order_by_asc(designation::Column::Id)
        .all(&db);
    let roles = role::Entity::find()
        .filter(role::Column::TenantId.eq(tenant))
        .filter(role::Column::IsDeleted.eq(false))
        .order_by_asc(role::Column::Name).order_by_asc(role::Column::Id)
        .all(&db);
    let managers = manager_query(tenant, search, manager_offset.unwrap_or(0) as u64).all(&db);
    let (departments, designations, roles, mut managers) = tokio::try_join!(departments, designations, roles, managers)
        .map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    let has_more = managers.len() > MANAGER_PAGE_SIZE as usize;
    managers.truncate(MANAGER_PAGE_SIZE as usize);
    Ok(Json(json!({
        "departments": departments.into_iter().map(|row| json!({"id": row.id, "name": row.name})).collect::<Vec<_>>(),
        "designations": designations.into_iter().map(|row| json!({"id": row.id, "title": row.title})).collect::<Vec<_>>(),
        "roles": roles.into_iter().map(|row| json!({"id": row.id, "name": row.name})).collect::<Vec<_>>(),
        "managers": managers.into_iter().map(|row| json!({"id": row.id, "fullName": format!("{} {}", row.first_name, row.last_name), "employeeCode": row.employee_code})).collect::<Vec<_>>(),
        "hasMoreManagers": has_more,
    })))
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Schema};
    use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
    use kabipay_common::subgraph::TenantId;
    use sea_orm::{DbBackend, QueryTrait};
    use std::collections::HashMap;

    struct TestQuery;
    #[Object]
    impl TestQuery {
        async fn options(&self, ctx: &Context<'_>) -> Result<Json<Value>> {
            super::options(ctx, None, None).await
        }
    }

    #[tokio::test]
    async fn conversion_directory_requires_all_conversion_privileges_before_database_access() {
        for permissions in [vec!["prejoining:review"], vec!["prejoining:review", "role:manage"], vec!["employee:manage", "role:manage"]] {
            let tenant = Uuid::new_v4();
            let claims = ClientClaims {
                sub: Uuid::new_v4(), iss: CLIENT_JWT_ISSUER.into(), exp: 0, iat: 0,
                tenant_id: tenant, email: String::new(), employee_id: None,
                must_change_password: false, roles: vec!["HR".into()],
                permission_scopes: permissions.iter().map(|permission| (permission.to_string(), "ALL".to_string())).collect(),
                permissions: permissions.into_iter().map(str::to_owned).collect(),
                resource_scopes: HashMap::new(),
            };
            let response = Schema::build(TestQuery, EmptyMutation, EmptySubscription)
                .data(TenantId(tenant)).data(claims).finish().execute("{ options }").await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("permission"), "{response:?}");
            assert!(!response.errors[0].message.contains("database"));
        }
    }

    #[test]
    fn manager_search_and_later_pages_keep_tenant_and_active_filters() {
        let tenant = Uuid::new_v4();
        let statement = manager_query(tenant, "O'Neil", 150).build(DbBackend::Postgres);
        assert!(statement.sql.contains("tenant_id"));
        assert!(statement.sql.contains("is_deleted"));
        assert!(statement.sql.contains("status"));
        assert!(statement.sql.contains("LIMIT"));
        assert!(statement.sql.contains("OFFSET"));
        assert!(!statement.sql.contains("O'Neil"));
        let values = statement.values.expect("bound values").0;
        assert!(values.contains(&sea_orm::Value::from(tenant)));
        assert!(values.contains(&sea_orm::Value::from(150_u64)));
        assert!(values.contains(&sea_orm::Value::from(51_u64)));
    }

    #[tokio::test]
    async fn employee_self_scope_cannot_open_conversion_choices() {
        for permission in ["employee:write", "employee:manage"] {
            let tenant = Uuid::new_v4();
            let claims: ClientClaims = serde_json::from_value(json!({
                "sub": Uuid::new_v4(), "iss": CLIENT_JWT_ISSUER, "exp": 0, "iat": 0,
                "tenant_id": tenant,
                "permissions": ["prejoining:review", "role:manage", permission],
                "permission_scopes": {
                    "prejoining:review": "ALL", "role:manage": "ALL", (permission): "SELF"
                }
            })).expect("valid test claims");
            let response = Schema::build(TestQuery, EmptyMutation, EmptySubscription)
                .data(TenantId(tenant)).data(claims).finish().execute("{ options }").await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("employee:"));
            assert!(!response.errors[0].message.contains("database"));
        }
    }
}
