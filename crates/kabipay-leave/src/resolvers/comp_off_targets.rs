use async_graphql::{Context, Result, SimpleObject, ID};
use kabipay_common::{client_data_scope::data_scope_from_claims, context::{ClientClaims, ScopeType, PERM_LEAVE_MANAGE}, subgraph::{require_tenant_id, tenant_db}, KabiPayError};
use kabipay_db_entities::tenant::{d0006_org_hierarchy::designation, d0007_employee_core::employee};
use sea_orm::{ColumnTrait, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;

#[derive(SimpleObject)]
pub struct CompOffEmployeeOption { pub id: ID, pub employee_code: String, pub full_name: String }
#[derive(SimpleObject)]
pub struct CompOffDesignationOption { pub id: ID, pub title: String }
#[derive(SimpleObject)]
pub struct CompOffPolicyTargets { pub employees: Vec<CompOffEmployeeOption>, pub designations: Vec<CompOffDesignationOption> }

pub async fn load(ctx: &Context<'_>) -> Result<CompOffPolicyTargets> {
    let scope = data_scope_from_claims(ctx.data_opt::<ClientClaims>(), PERM_LEAVE_MANAGE).map_err(KabiPayError::into_graphql)?;
    if scope != ScopeType::All { return Err(KabiPayError::Forbidden("leave:manage requires ALL scope".into()).into_graphql()); }
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    // Only selector labels are exposed by policy administration, never employee profile fields.
    let employees = employee::Entity::find().select_only()
        .columns([employee::Column::Id, employee::Column::EmployeeCode, employee::Column::FirstName, employee::Column::LastName])
        .filter(employee::Column::TenantId.eq(tenant)).filter(employee::Column::IsDeleted.eq(false))
        .order_by_asc(employee::Column::EmployeeCode).into_tuple::<(Uuid, String, String, String)>()
        .all(&db).await.map_err(|error| KabiPayError::from(error).into_graphql())?;
    let designations = designation::Entity::find().select_only().columns([designation::Column::Id, designation::Column::Title])
        .filter(designation::Column::TenantId.eq(tenant)).filter(designation::Column::IsDeleted.eq(false))
        .order_by_asc(designation::Column::Title).into_tuple::<(Uuid, String)>()
        .all(&db).await.map_err(|error| KabiPayError::from(error).into_graphql())?;
    Ok(CompOffPolicyTargets {
        employees: employees.into_iter().map(|(id, code, first, last)| CompOffEmployeeOption { id: ID(id.to_string()), employee_code: code, full_name: format!("{first} {last}").trim().to_owned() }).collect(),
        designations: designations.into_iter().map(|(id, title)| CompOffDesignationOption { id: ID(id.to_string()), title }).collect(),
    })
}
