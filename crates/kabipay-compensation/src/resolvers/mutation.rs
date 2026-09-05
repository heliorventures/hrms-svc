use async_graphql::{Context, InputObject, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    context::ScopeType,
    KabiPayError,
};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, QuerySelect,
    TransactionTrait, Set,
};
use kabipay_db_entities::tenant::d0021_compensation::{compensation_review_cycle, salary_band};
use uuid::Uuid;
use super::types::*;

pub struct MutationRoot;

fn invalid(message: &str) -> async_graphql::Error {
    KabiPayError::Validation(message.into()).into_graphql()
}

#[derive(InputObject)]
pub struct SaveCompensationReviewCycleInput {
    pub id: Option<Uuid>,
    pub name: String,
    pub year: i32,
    pub start_date: chrono::NaiveDate,
    pub end_date: chrono::NaiveDate,
    pub budget_percentage: Option<String>,
}

#[derive(InputObject)]
pub struct SaveSalaryBandInput {
    pub id: Option<Uuid>,
    pub designation_id: uuid::Uuid,
    pub grade: Option<i32>,
    pub min_salary: Option<String>,
    pub mid_salary: Option<String>,
    pub max_salary: Option<String>,
    pub currency: Option<String>,
    pub effective_year: Option<i32>,
}

#[Object]
impl MutationRoot {
    async fn save_compensation_review_cycle(
        &self,
        ctx: &Context<'_>,
        input: SaveCompensationReviewCycleInput,
    ) -> Result<CompensationReviewCycleDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["compensation:manage"])
            || claims.scope_for_permission("compensation:manage") != Some(ScopeType::All)
        {
            return Err(
                KabiPayError::Forbidden("compensation:manage with ALL scope required".into())
                    .into_graphql(),
            );
        }
        let name = input.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(invalid("Name must contain 1 to 200 characters"));
        }
        if input.start_date > input.end_date || !(1900..=9999).contains(&input.year) {
            return Err(invalid("Enter a valid year and date range"));
        }
        let budget_percentage = decimal(input.budget_percentage.as_deref(), 11)?;
        let connection = tenant_db(ctx, tenant_id).await?;
        let db = connection
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let existing = if let Some(id) = input.id {
            Some(compensation_review_cycle::Entity::find_by_id(id)
                .filter(compensation_review_cycle::Column::TenantId.eq(tenant_id))
                .lock_exclusive()
                .one(&db)
                .await
                .map_err(KabiPayError::from)
                .map_err(KabiPayError::into_graphql)?
                .ok_or_else(|| invalid("Record not found"))?)
        } else {
            None
        };
        if existing.as_ref().is_some_and(|v| v.status != "DRAFT") {
            return Err(invalid("Only draft review cycle setup can be edited"));
        }
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| compensation_review_cycle::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.year = Set(input.year);
        model.start_date = Set(input.start_date);
        model.end_date = Set(input.end_date);
        model.budget_percentage = Set(budget_percentage);
        if is_new {
            model.status = Set("DRAFT".to_string());
        }
        model.updated_at = Set(chrono::Utc::now());
        let saved = if is_new {
            model.insert(&db).await
        } else {
            model.update(&db).await
        }
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
        db.commit()
            .await
            .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
    }

    async fn save_salary_band(
        &self,
        ctx: &Context<'_>,
        input: SaveSalaryBandInput,
    ) -> Result<SalaryBandDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["compensation:manage"])
            || claims.scope_for_permission("compensation:manage") != Some(ScopeType::All)
        {
            return Err(
                KabiPayError::Forbidden("compensation:manage with ALL scope required".into())
                    .into_graphql(),
            );
        }
        let min_salary = decimal(input.min_salary.as_deref(), 11)?;
        let mid_salary = decimal(input.mid_salary.as_deref(), 11)?;
        let max_salary = decimal(input.max_salary.as_deref(), 11)?;
        if min_salary.zip(mid_salary).is_some_and(|(a, b)| a > b)
            || mid_salary.zip(max_salary).is_some_and(|(a, b)| a > b)
            || min_salary.zip(max_salary).is_some_and(|(a, b)| a > b) {
            return Err(invalid("Salary values must be ordered minimum, midpoint, maximum"));
        }
        if input.grade.is_some_and(|v| v < 0)
            || input.effective_year.is_some_and(|v| !(1900..=9999).contains(&v)) {
            return Err(invalid("Enter a valid grade and year"));
        }
        let currency = input.currency.map(|v| v.trim().to_ascii_uppercase())
            .filter(|v| !v.is_empty());
        if currency.as_ref().is_some_and(|v| v.len() != 3
            || !v.bytes().all(|c| c.is_ascii_uppercase())) {
            return Err(invalid("Currency must be a three-letter code"));
        }
        let connection = tenant_db(ctx, tenant_id).await?;
        let db = connection
            .begin()
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        use kabipay_db_entities::tenant::d0006_org_hierarchy::designation;
        if designation::Entity::find_by_id(input.designation_id)
            .filter(designation::Column::TenantId.eq(tenant_id))
            .one(&db)
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?
            .is_none()
        {
            return Err(invalid("Select a designation in this organization"));
        }
        let existing = if let Some(id) = input.id {
            Some(salary_band::Entity::find_by_id(id)
                .filter(salary_band::Column::TenantId.eq(tenant_id))
                .one(&db)
                .await
                .map_err(KabiPayError::from)
                .map_err(KabiPayError::into_graphql)?
                .ok_or_else(|| invalid("Record not found"))?)
        } else {
            None
        };
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| salary_band::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.designation_id = Set(input.designation_id);
        model.grade = Set(input.grade);
        model.min_salary = Set(min_salary);
        model.mid_salary = Set(mid_salary);
        model.max_salary = Set(max_salary);
        model.currency = Set(currency);
        model.effective_year = Set(input.effective_year);
        model.updated_at = Set(chrono::Utc::now());
        let saved = if is_new {
            model.insert(&db).await
        } else {
            model.update(&db).await
        }
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
        db.commit()
            .await
            .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
    }
}

fn decimal(value: Option<&str>, integer_digits: usize) -> Result<Option<sea_orm::prelude::Decimal>> {
    use std::str::FromStr;
    let Some(value) = value.map(str::trim).filter(|v| !v.is_empty()) else {
        return Ok(None);
    };
    let parts: Vec<_> = value.split('.').collect();
    if parts.len() > 2
        || parts[0].is_empty()
        || parts[0].len() > integer_digits
        || parts.iter().any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()))
        || parts.get(1).is_some_and(|p| p.len() > 4) {
        return Err(invalid("Enter a nonnegative decimal with at most four decimal places"));
    }
    sea_orm::prelude::Decimal::from_str(value).map(Some)
        .map_err(|_| invalid("Invalid decimal value"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decimal_validation() {
        assert!(decimal(Some("NaN"), 13).is_err());
        assert!(decimal(Some("-1"), 13).is_err());
        assert!(decimal(Some("1.00001"), 13).is_err());
        assert!(decimal(Some("1000"), 3).is_err());
        assert!(decimal(Some("0.25"), 13).is_ok());
    }
}

#[cfg(test)]
mod authorization_tests {
    use super::*;
    use async_graphql::{EmptySubscription, Request, Schema};
    use kabipay_common::{context::ClientClaims, subgraph::TenantId};

    #[tokio::test]
    async fn setup_requires_permission_and_its_all_scope_before_database_access() {
        let schema = Schema::build(crate::resolvers::QueryRoot, MutationRoot, EmptySubscription).finish();
        for (permissions, scopes) in [
            (vec!["compensation:manage"], serde_json::json!({"compensation:manage":"TEAM"})),
            (vec!["compensation:manage"], serde_json::json!({"compensation:read":"ALL"})),
            (vec![], serde_json::json!({"compensation:manage":"ALL"})),
        ] {
            let claims: ClientClaims = serde_json::from_value(serde_json::json!({
                "sub": Uuid::nil(),
                "iss": "test",
                "exp": 0,
                "iat": 0,
                "tenant_id": Uuid::nil(),
                "email": "test@example.com",
                "roles": [],
                "permissions": permissions,
                "permission_scopes": scopes,
                "resource_scopes": {}
            }))
            .expect("test claims");
            let response = schema
                .execute(Request::new(r#"mutation { saveCompensationReviewCycle(input: {name: "Annual", year: 2026, startDate: "2026-01-01", endDate: "2026-12-31"}) { id } }"#)
                .data(TenantId(Uuid::nil()))
                .data(claims),
                )
                .await;
            assert_eq!(response.errors.len(), 1);
            assert!(
                response.errors[0].message.contains("ALL scope required"),
                "{:?}",
                response.errors
            );
        }
    }
}
