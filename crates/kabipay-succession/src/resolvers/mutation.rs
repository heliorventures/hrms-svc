use async_graphql::{Context, InputObject, Object, Result};
use kabipay_common::{
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    context::ScopeType,
    KabiPayError,
};
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use kabipay_db_entities::tenant::d0020_succession::{competency, talent_pool};
use uuid::Uuid;
use super::types::*;

pub struct MutationRoot;

fn invalid(message: &str) -> async_graphql::Error {
    KabiPayError::Validation(message.into()).into_graphql()
}

#[derive(InputObject)]
pub struct SaveCompetencyInput {
    pub id: Option<Uuid>,
    pub name: String,
    pub category: Option<String>,
    pub description: Option<String>,
}

#[derive(InputObject)]
pub struct SaveTalentPoolInput {
    pub id: Option<Uuid>,
    pub name: String,
    pub description: Option<String>,
}

#[Object]
impl MutationRoot {
    async fn save_competency(
        &self,
        ctx: &Context<'_>,
        input: SaveCompetencyInput,
    ) -> Result<CompetencyDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["succession:manage"])
            || claims.scope_for_permission("succession:manage") != Some(ScopeType::All)
        {
            return Err(
                KabiPayError::Forbidden("succession:manage with ALL scope required".into())
                    .into_graphql(),
            );
        }
        let name = input.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(invalid("Name must contain 1 to 200 characters"));
        }
        if input.category.as_ref().is_some_and(|v| v.chars().count() > 100) {
            return Err(invalid("Category must be at most 100 characters"));
        }
        if input.description.as_ref().is_some_and(|v| v.chars().count() > 10000) {
            return Err(invalid("Description is too long"));
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let existing = if let Some(id) = input.id {
            Some(competency::Entity::find_by_id(id)
                .filter(competency::Column::TenantId.eq(tenant_id))
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
            .unwrap_or_else(|| competency::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.category = Set(input.category.map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()));
        model.description = Set(input.description.map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()));
        model.updated_at = Set(chrono::Utc::now());
        let saved = if is_new {
            model.insert(&db).await
        } else {
            model.update(&db).await
        }
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
    }

    async fn save_talent_pool(
        &self,
        ctx: &Context<'_>,
        input: SaveTalentPoolInput,
    ) -> Result<TalentPoolDto> {
        let tenant_id = require_tenant_id(ctx)?;
        let claims = require_client_claims(ctx)?;
        if !claims.has_any_permission(&["succession:manage"])
            || claims.scope_for_permission("succession:manage") != Some(ScopeType::All)
        {
            return Err(
                KabiPayError::Forbidden("succession:manage with ALL scope required".into())
                    .into_graphql(),
            );
        }
        let name = input.name.trim().to_string();
        if name.is_empty() || name.chars().count() > 200 {
            return Err(invalid("Name must contain 1 to 200 characters"));
        }
        if input.description.as_ref().is_some_and(|v| v.chars().count() > 10000) {
            return Err(invalid("Description is too long"));
        }
        let db = tenant_db(ctx, tenant_id).await?;
        let existing = if let Some(id) = input.id {
            Some(talent_pool::Entity::find_by_id(id)
                .filter(talent_pool::Column::TenantId.eq(tenant_id))
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
            .unwrap_or_else(|| talent_pool::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_at: Set(chrono::Utc::now()),
                ..Default::default()
            });
        model.name = Set(name);
        model.description = Set(input.description.map(|v| v.trim().to_string())
            .filter(|v| !v.is_empty()));
        model.updated_at = Set(chrono::Utc::now());
        let saved = if is_new {
            model.insert(&db).await
        } else {
            model.update(&db).await
        }
        .map_err(KabiPayError::from)
        .map_err(KabiPayError::into_graphql)?;
        Ok(saved.into())
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
            (vec!["succession:manage"], serde_json::json!({"succession:manage":"TEAM"})),
            (vec!["succession:manage"], serde_json::json!({"succession:read":"ALL"})),
            (vec![], serde_json::json!({"succession:manage":"ALL"})),
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
                .execute(Request::new(r#"mutation { saveTalentPool(input: {name: "Leaders"}) { id } }"#)
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
