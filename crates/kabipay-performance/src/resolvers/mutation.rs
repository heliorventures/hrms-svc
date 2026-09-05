use async_graphql::{Context, InputObject, Object, Result, ID};
use kabipay_common::{
    context::ScopeType,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0018_performance::{review_cycle};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
    QuerySelect, TransactionTrait,
};
use uuid::Uuid;

use super::types::{ReviewCycleDto};

fn validate_text(value: &str, maximum: usize) -> Result<String> {
    let value = value.trim();
    if value.is_empty() || value.chars().count() > maximum {
        return Err(KabiPayError::Validation(format!(
            "Text must contain 1 to {maximum} characters"
        )).into_graphql());
    }
    Ok(value.to_owned())
}

fn optional_text(value: Option<String>, maximum: usize) -> Result<Option<String>> {
    value
        .filter(|value| !value.trim().is_empty())
        .map(|value| validate_text(&value, maximum))
        .transpose()
}

#[derive(InputObject)]
pub struct SaveReviewCycleInput {
    pub id: Option<ID>,
    pub name: String,
    pub start_date: chrono::NaiveDate,
    pub end_date: chrono::NaiveDate,
    pub review_type: Option<String>,
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn save_review_cycle(
        &self,
        ctx: &Context<'_>,
        input: SaveReviewCycleInput,
    ) -> Result<ReviewCycleDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["performance:manage"])
            || claims.explicit_scope_for_permission("performance:manage") != Some(ScopeType::All)
        {
            return Err(KabiPayError::Forbidden(
                "performance:manage with ALL scope required".into(),
            ).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let name = validate_text(&input.name, 255)?;
        let review_type = optional_text(input.review_type, 50)?;
        if input.end_date < input.start_date {
            return Err(KabiPayError::Validation(
                "End date must be on or after start date".into(),
            ).into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let txn = db.begin().await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        let existing = match input.id {
            Some(id) => {
                let id = Uuid::parse_str(id.as_str()).map_err(|_| {
                    KabiPayError::Validation("Invalid ID".into()).into_graphql()
                })?;
                Some(review_cycle::Entity::find_by_id(id)
                    .filter(review_cycle::Column::TenantId.eq(tenant_id))
                    .lock_exclusive()
                    .one(&txn)
                    .await
                    .map_err(KabiPayError::from)
                    .map_err(KabiPayError::into_graphql)?
                    .ok_or_else(|| KabiPayError::NotFound {
                        entity: "catalog record",
                        id: id.to_string(),
                    }.into_graphql())?)
            }
            None => None,
        };
        if existing.as_ref().is_some_and(|row| !row.status.eq_ignore_ascii_case("DRAFT")) {
            return Err(KabiPayError::Validation(
                "Only draft review cycles can be edited".into(),
            ).into_graphql());
        }
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| review_cycle::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.start_date = Set(input.start_date);
        model.end_date = Set(input.end_date);
        model.review_type = Set(review_type);
        if is_new {
            model.status = Set("DRAFT".into());
        }
        model.updated_at = Set(chrono::Utc::now());
        let row = if is_new {
            model.insert(&txn).await
        } else {
            model.update(&txn).await
        }.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        txn.commit().await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)?;
        Ok(row.into())
    }

}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_blank_and_overlong_names() {
        assert!(validate_text("  ", 255).is_err());
        assert!(validate_text(&"a".repeat(256), 255).is_err());
        assert_eq!(validate_text(" Skills ", 255).unwrap(), "Skills");
    }

    #[tokio::test]
    async fn rejects_missing_or_narrow_manage_scope_before_database_access() {
        for scope in ["SELF", "TEAM", "DEPARTMENT", ""] {
            let claims: kabipay_common::context::ClientClaims =
                serde_json::from_value(serde_json::json!({
                    "sub": Uuid::new_v4(),
                    "tenant_id": Uuid::new_v4(),
                    "iss": "kabipay-client",
                    "iat": 0,
                    "exp": 9999999999i64,
                    "permissions": ["performance:manage"],
                    "permission_scopes": {"performance:manage": scope}
                })).unwrap();
            let schema = async_graphql::Schema::build(
                crate::resolvers::QueryRoot,
                MutationRoot,
                async_graphql::EmptySubscription,
            ).data(claims).finish();
            let response = schema.execute(
                r#"mutation { saveReviewCycle(input: {name: "Annual", startDate: "2026-01-01", endDate: "2026-12-31"}) { id } }"#,
            ).await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("ALL scope required"));
        }
    }
}
