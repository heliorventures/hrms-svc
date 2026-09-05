use async_graphql::{Context, InputObject, Object, Result, ID};
use chrono::{NaiveDate, Utc};
use kabipay_common::{
    context::ScopeType,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
    KabiPayError,
};
use kabipay_db_entities::tenant::d0016_recruitment::job_posting;
use sea_orm::{ActiveModelTrait, ColumnTrait, EntityTrait, IntoActiveModel, QueryFilter, Set};
use uuid::Uuid;

use super::types::JobPostingDto;

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
pub struct JobPostingInput {
    pub title: String,
    pub description: Option<String>,
    pub employment_type: Option<String>,
    pub vacancies: i32,
    pub status: String,
    pub open_date: Option<NaiveDate>,
    pub close_date: Option<NaiveDate>,
}

impl JobPostingInput {
    fn validate(&self) -> Result<()> {
        required(&self.title, 500)?;
        if self.vacancies < 1 {
            return Err(invalid("vacancies must be positive"));
        }
        if !matches!(self.status.as_str(), "DRAFT" | "OPEN" | "CLOSED" | "ON_HOLD") {
            return Err(invalid("invalid job status"));
        }
        if matches!((self.open_date, self.close_date), (Some(a), Some(b)) if b < a) {
            return Err(invalid("close date must not precede open date"));
        }
        Ok(())
    }
}

pub struct MutationRoot;

#[Object]
impl MutationRoot {
    async fn save_job_posting(
        &self,
        ctx: &Context<'_>,
        id: Option<ID>,
        input: JobPostingInput,
    ) -> Result<JobPostingDto> {
        let claims = require_client_claims(ctx)?;
        if !claims.can_manage_recruitment()
            || claims.explicit_scope_for_permission("recruitment:manage") != Some(ScopeType::All)
        {
            return Err(KabiPayError::Forbidden(
                "recruitment:manage with ALL scope required".into(),
            )
            .into_graphql());
        }
        input.validate()?;
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let editing = id.is_some();
        let mut model = if let Some(id) = id {
            job_posting::Entity::find_by_id(uuid(&id)?)
                .filter(job_posting::Column::TenantId.eq(tenant_id))
                .one(&db)
                .await
                .map_err(KabiPayError::from)
                .map_err(KabiPayError::into_graphql)?
                .ok_or_else(|| invalid("job posting not found"))?
                .into_active_model()
        } else {
            job_posting::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant_id),
                created_by: Set(Some(claims.sub)),
                created_at: Set(Utc::now()),
                ..Default::default()
            }
        };
        model.title = Set(required(&input.title, 500)?);
        model.description = Set(optional(input.description, 20000)?);
        model.employment_type = Set(optional(input.employment_type, 50)?);
        model.vacancies = Set(input.vacancies);
        model.status = Set(input.status);
        model.open_date = Set(input.open_date);
        model.close_date = Set(input.close_date);
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
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_invalid_job_inputs() {
        let mut i = JobPostingInput {
            title: "Engineer".into(),
            description: None,
            employment_type: None,
            vacancies: 0,
            status: "OPEN".into(),
            open_date: None,
            close_date: None,
        };
        assert!(i.validate().is_err());
        i.vacancies = 1;
        assert!(i.validate().is_ok());
        i.status = "UNKNOWN".into();
        assert!(i.validate().is_err());
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
                vec!["recruitment:manage"]
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
                    "permission_scopes": {"recruitment:manage": scope},
                    "resource_scopes": {"recruitment": "ALL"}
                }))
                .unwrap();
            let schema = async_graphql::Schema::build(
                crate::resolvers::QueryRoot,
                crate::resolvers::MutationRoot,
                async_graphql::EmptySubscription,
            )
            .data(claims)
            .finish();
            for query in [r#"mutation { saveJobPosting(input: {title: "Engineer", vacancies: 1, status: "DRAFT"}) { id } }"#] {
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
