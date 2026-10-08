use async_graphql::{Context, InputObject, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::{
    context::ScopeType,
    entitlements::{permission_module, Entitlements},
    subgraph::require_client_claims,
    KabiPayError, PageInfo,
};
use kabipay_db_entities::tenant::d0006_org_hierarchy::location;

#[derive(SimpleObject)]
pub struct CompanyLocation {
    pub id: ID,
    pub name: String,
    pub address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
    pub active: bool,
    pub updated_at: DateTime<Utc>,
}
impl From<location::Model> for CompanyLocation {
    fn from(row: location::Model) -> Self {
        Self {
            id: row.id.into(),
            name: row.name,
            address: row.address,
            city: row.city,
            state: row.state,
            country: row.country,
            active: !row.is_deleted,
            updated_at: row.updated_at,
        }
    }
}
#[derive(SimpleObject)]
pub struct CompanyLocationPage {
    pub nodes: Vec<CompanyLocation>,
    pub page_info: PageInfo,
}
#[derive(SimpleObject)]
pub struct CompanyLocationOption {
    pub id: ID,
    pub name: String,
}
#[derive(SimpleObject)]
pub struct EmployeeLocationAssignment {
    pub employee_id: ID,
    pub location_id: Option<ID>,
    pub location_name: Option<String>,
    pub effective_from: Option<NaiveDate>,
    pub revision: i64,
    pub business_date: NaiveDate,
}
#[derive(InputObject)]
pub struct SaveCompanyLocationInput {
    pub id: Option<ID>,
    pub expected_updated_at: Option<DateTime<Utc>>,
    pub name: String,
    pub address: Option<String>,
    pub city: Option<String>,
    pub state: Option<String>,
    pub country: Option<String>,
}
#[derive(InputObject)]
pub struct AssignEmployeeLocationInput {
    pub employee_id: ID,
    pub location_id: Option<ID>,
    pub effective_date: NaiveDate,
    pub expected_revision: i64,
}

pub fn require_location_authority(ctx: &Context<'_>, options: bool) -> async_graphql::Result<()> {
    let claims = require_client_claims(ctx)?;
    let management = ["employee:manage", "employee:write"];
    let choices = if options {
        &[
            "employee:manage",
            "employee:write",
            "employee:read",
            "leave:manage",
            "attendance:punch_policy",
            "expense:read",
            "travel:read",
        ][..]
    } else {
        &management[..]
    };
    let entitlement = ctx.data_opt::<Entitlements>();
    for permission in choices {
        if claims.has_any_permission(&[permission])
            && claims.scope_for_permission(permission) == Some(ScopeType::All)
        {
            let state = entitlement.ok_or_else(|| {
                KabiPayError::Internal("request entitlement snapshot missing".into()).into_graphql()
            })?;
            state
                .require_tenant(claims.tenant_id)
                .and_then(|_| state.require(permission_module(permission)))
                .map_err(KabiPayError::into_graphql)?;
            return Ok(());
        }
    }
    Err(KabiPayError::Forbidden(
        "company locations require an authorized permission with ALL scope".into(),
    )
    .into_graphql())
}
