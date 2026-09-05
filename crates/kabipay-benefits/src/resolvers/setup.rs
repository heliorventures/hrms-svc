use std::str::FromStr;

use async_graphql::{Context, InputObject, Result, ID};
use chrono::Utc;
use kabipay_common::{
    context::ScopeType,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0014_benefits::{benefit_plan, benefit_type};
use sea_orm::{
    prelude::Decimal, ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use uuid::Uuid;

use super::types::{BenefitPlanDto, BenefitTypeDto};

fn invalid(message: &str) -> async_graphql::Error {
    KabiPayError::Validation(message.into()).into_graphql()
}

fn required(value: &str, max: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > max {
        return Err(invalid("required text is empty or too long"));
    }
    Ok(value.into())
}

fn optional(value: Option<String>, max: usize) -> Result<Option<String>> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(|v| required(&v, max))
        .transpose()
}

fn uuid(value: &ID) -> Result<Uuid> {
    Uuid::parse_str(value.as_str()).map_err(|_| invalid("invalid identifier"))
}

#[derive(InputObject)]
pub struct BenefitTypeInput {
    pub name: String,
    pub code: String,
    pub category: Option<String>,
}

#[derive(InputObject)]
pub struct BenefitPlanInput {
    pub name: String,
    pub benefit_type_id: ID,
    pub employer_contribution: Option<String>,
    pub employee_contribution: Option<String>,
    pub contribution_type: Option<String>,
    pub is_mandatory: bool,
    pub is_active: bool,
}

fn amount(value: Option<String>) -> Result<Option<Decimal>> {
    value
        .filter(|v| !v.trim().is_empty())
        .map(|v| {
            let d = Decimal::from_str(v.trim())
                .map_err(|_| invalid("invalid contribution amount"))?;
            if d < Decimal::ZERO || d.scale() > 4 || d >= Decimal::from(100_000_000_000i64) {
                return Err(invalid(
                    "contribution must be nonnegative with at most four decimal places and eleven integer digits",
                ));
            }
            Ok(d)
        })
        .transpose()
}

pub async fn save_type(
    ctx: &Context<'_>,
    id: Option<ID>,
    input: BenefitTypeInput,
) -> Result<BenefitTypeDto> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_benefits_catalog()
        || claims.explicit_scope_for_permission("benefits:manage") != Some(ScopeType::All)
    {
        return Err(KabiPayError::Forbidden(
            "benefits:manage with ALL scope required".into(),
        )
        .into_graphql());
    }
    let name = required(&input.name, 255)?;
    let code = required(&input.code, 50)?.to_ascii_uppercase();
    let category = optional(input.category, 100)?;
    let tenant_id = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant_id).await?;
    let id = id.as_ref().map(uuid).transpose()?;
    let duplicate = benefit_type::Entity::find()
        .filter(benefit_type::Column::TenantId.eq(tenant_id))
        .filter(benefit_type::Column::Code.eq(&code))
        .one(&db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
    if duplicate.is_some_and(|row| Some(row.id) != id) {
        return Err(KabiPayError::Conflict("benefit type code already exists".into()).into_graphql());
    }
    let mut model = if let Some(id) = id {
        benefit_type::Entity::find_by_id(id)
            .filter(benefit_type::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| invalid("benefit type not found"))?
            .into_active_model()
    } else {
        benefit_type::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
    };
    model.name = Set(name);
    model.code = Set(code);
    model.category = Set(category);
    model.updated_at = Set(Utc::now());
    let row = if id.is_some() {
        model.update(&db).await
    } else {
        model.insert(&db).await
    }
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?;
    Ok(row.into())
}

pub async fn save_plan(
    ctx: &Context<'_>,
    id: Option<ID>,
    input: BenefitPlanInput,
) -> Result<BenefitPlanDto> {
    let claims = require_client_claims(ctx)?;
    if !claims.can_manage_benefits_catalog()
        || claims.explicit_scope_for_permission("benefits:manage") != Some(ScopeType::All)
    {
        return Err(KabiPayError::Forbidden(
            "benefits:manage with ALL scope required".into(),
        )
        .into_graphql());
    }
    let name = required(&input.name, 255)?;
    let employer = amount(input.employer_contribution)?;
    let employee = amount(input.employee_contribution)?;
    let contribution_type = optional(input.contribution_type, 50)?;
    let tenant_id = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant_id).await?;
    let type_id = uuid(&input.benefit_type_id)?;
    if benefit_type::Entity::find_by_id(type_id)
        .filter(benefit_type::Column::TenantId.eq(tenant_id))
        .one(&db)
        .await
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?
        .is_none()
    {
        return Err(invalid("benefit type not found"));
    }
    let editing = id.is_some();
    let mut model = if let Some(id) = id {
        benefit_plan::Entity::find_by_id(uuid(&id)?)
            .filter(benefit_plan::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .ok_or_else(|| invalid("benefit plan not found"))?
            .into_active_model()
    } else {
        benefit_plan::ActiveModel {
            id: Set(Uuid::new_v4()),
            tenant_id: Set(tenant_id),
            created_at: Set(Utc::now()),
            ..Default::default()
        }
    };
    model.name = Set(name);
    model.benefit_type_id = Set(type_id);
    model.employer_contribution = Set(employer);
    model.employee_contribution = Set(employee);
    model.contribution_type = Set(contribution_type);
    model.is_mandatory = Set(input.is_mandatory);
    model.is_active = Set(input.is_active);
    model.updated_at = Set(Utc::now());
    let row = if editing {
        model.update(&db).await
    } else {
        model.insert(&db).await
    }
    .map_err(KabiPayError::from)
    .map_err(KabiPayError::into_graphql)?;
    Ok(row.into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contribution_precision_and_range() {
        for v in ["-1", "1.12345", "100000000000", "NaN"] {
            assert!(amount(Some(v.into())).is_err());
        }
        assert!(amount(Some("99999999999.9999".into())).is_ok());
        assert_eq!(amount(Some(" ".into())).unwrap(), None);
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;

    #[tokio::test]
    async fn denies_missing_grants_and_narrow_scopes_before_database_access() {
        for (grant, scope) in [
            (true, "SELF"),
            (true, "TEAM"),
            (true, "DEPARTMENT"),
            (true, ""),
            (false, "ALL"),
        ] {
            let permissions: Vec<&str> = if grant {
                vec!["benefits:manage"]
            } else {
                vec![]
            };
            let claims: kabipay_common::context::ClientClaims =
                serde_json::from_value(serde_json::json!({
                    "sub": Uuid::new_v4(),
                    "tenant_id": Uuid::new_v4(),
                    "iss": "kabipay-client",
                    "iat": 0,
                    "exp": 9999999999i64,
                    "permissions": permissions,
                    "permission_scopes": {"benefits:manage": scope},
                    "resource_scopes": {"benefits": "ALL"}
                }))
                .unwrap();
            let schema = async_graphql::Schema::build(
                crate::resolvers::QueryRoot,
                crate::resolvers::MutationRoot,
                async_graphql::EmptySubscription,
            )
            .data(claims)
            .finish();
            for query in [
                r#"mutation { saveBenefitType(input: {name: "Medical", code: "MED"}) { id } }"#,
                r#"mutation { saveBenefitPlan(input: {name: "Medical", benefitTypeId: "00000000-0000-0000-0000-000000000001", isActive: true, isMandatory: false}) { id } }"#,
            ] {
                let response = schema.execute(query).await;
                assert_eq!(response.errors.len(), 1);
                assert!(
                    response.errors[0].message.contains("ALL scope required"),
                    "{:?}",
                    response.errors
                );
            }
        }
    }
}
