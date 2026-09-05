use async_graphql::{Context, InputObject, Object, Result, ID};
use kabipay_common::{
    context::ScopeType,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0019_lms::{skill, course};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set,
};
use uuid::Uuid;

use super::types::{SkillDto, CourseDto};

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
pub struct SaveSkillInput {
    pub id: Option<ID>,
    pub name: String,
    pub category: Option<String>,
    pub level: Option<String>,
}

#[derive(InputObject)]
pub struct SaveCourseInput {
    pub id: Option<ID>,
    pub title: String,
    pub category: Option<String>,
    pub delivery_mode: Option<String>,
    pub duration_minutes: Option<i32>,
    pub is_mandatory: bool,
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn save_skill(
        &self,
        ctx: &Context<'_>,
        input: SaveSkillInput,
    ) -> Result<SkillDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["learning:manage"])
            || claims.explicit_scope_for_permission("learning:manage") != Some(ScopeType::All)
        {
            return Err(KabiPayError::Forbidden(
                "learning:manage with ALL scope required".into(),
            ).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let name = validate_text(&input.name, 255)?;
        let category = optional_text(input.category, 100)?;
        let level = optional_text(input.level, 50)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let existing = match input.id {
            Some(id) => {
                let id = Uuid::parse_str(id.as_str()).map_err(|_| {
                    KabiPayError::Validation("Invalid ID".into()).into_graphql()
                })?;
                Some(skill::Entity::find_by_id(id)
                    .filter(skill::Column::TenantId.eq(tenant_id))
                    .one(&db)
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
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| skill::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.category = Set(category);
        model.level = Set(level);
        model.updated_at = Set(chrono::Utc::now());
        let row = if is_new {
            model.insert(&db).await
        } else {
            model.update(&db).await
        }.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
        Ok(row.into())
    }

    async fn save_course(
        &self,
        ctx: &Context<'_>,
        input: SaveCourseInput,
    ) -> Result<CourseDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["learning:manage"])
            || claims.explicit_scope_for_permission("learning:manage") != Some(ScopeType::All)
        {
            return Err(KabiPayError::Forbidden(
                "learning:manage with ALL scope required".into(),
            ).into_graphql());
        }
        let tenant_id = require_tenant_id(ctx)?;
        let name = validate_text(&input.title, 500)?;
        let category = optional_text(input.category, 100)?;
        let delivery_mode = optional_text(input.delivery_mode, 50)?;
        if input.duration_minutes.is_some_and(|value| value <= 0) {
            return Err(KabiPayError::Validation("Duration must be positive".into()).into_graphql());
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let existing = match input.id {
            Some(id) => {
                let id = Uuid::parse_str(id.as_str()).map_err(|_| {
                    KabiPayError::Validation("Invalid ID".into()).into_graphql()
                })?;
                Some(course::Entity::find_by_id(id)
                    .filter(course::Column::TenantId.eq(tenant_id))
                    .one(&db)
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
        let is_new = existing.is_none();
        let mut model = existing
            .map(IntoActiveModel::into_active_model)
            .unwrap_or_else(|| course::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.title = Set(name);
        model.category = Set(category);
        model.delivery_mode = Set(delivery_mode);
        model.duration_minutes = Set(input.duration_minutes);
        model.is_mandatory = Set(input.is_mandatory);
        if is_new {
            model.is_active = Set(true);
            model.description = Set(None);
            model.created_by = Set(None);
        }
        model.updated_at = Set(chrono::Utc::now());
        let row = if is_new {
            model.insert(&db).await
        } else {
            model.update(&db).await
        }.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
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
                    "permissions": ["learning:manage"],
                    "permission_scopes": {"learning:manage": scope}
                })).unwrap();
            let schema = async_graphql::Schema::build(
                crate::resolvers::QueryRoot,
                MutationRoot,
                async_graphql::EmptySubscription,
            ).data(claims).finish();
            let response = schema.execute(
                r#"mutation { saveSkill(input: {name: "Security"}) { id } }"#,
            ).await;
            assert_eq!(response.errors.len(), 1);
            assert!(response.errors[0].message.contains("ALL scope required"));
        }
    }
}
