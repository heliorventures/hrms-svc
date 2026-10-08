//! Administrative configuration only; never call from respondent/result paths.
use async_graphql::{SimpleObject, ID};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0006_org_hierarchy::{department, location}, d0007_employee_core::employee,
    d0076_anonymous_surveys::survey_audience_department,
    d0084_survey_targeting_corrections::survey_audience_scope,
};
use sea_orm::{ColumnTrait, Condition, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect};
use uuid::Uuid;
use super::{survey_service, survey_targeting};

#[derive(SimpleObject)]
pub struct SurveyAudience {
    pub audience_kind: String,
    pub department_ids: Vec<ID>,
    pub location_ids: Vec<ID>,
    pub employee_ids: Vec<ID>,
    pub source_survey_id: Option<ID>,
}

#[derive(SimpleObject)]
pub struct SurveyAudienceOption { pub id: ID, pub label: String }

#[derive(SimpleObject)]
pub struct SurveyAudienceOptions { pub nodes: Vec<SurveyAudienceOption>, pub next_cursor: Option<ID> }

fn audience_kind(saved: Option<String>, has_departments: bool, extra: &survey_targeting::AudienceExtensions) -> KabiPayResult<String> {
    match saved.as_deref() {
        Some(kind @ ("ALL" | "DEPARTMENT" | "LOCATION" | "EMPLOYEE")) => Ok(kind.to_owned()),
        None if has_departments && extra.location_ids.is_empty() && extra.employee_ids.is_empty() => Ok("DEPARTMENT".into()),
        _ => Err(KabiPayError::Validation("Survey audience is incomplete; review the draft audience before saving".into())),
    }
}

pub async fn audience(db: &DatabaseConnection, tenant: Uuid, survey: Uuid) -> KabiPayResult<SurveyAudience> {
    survey_service::load_survey_model(db, tenant, survey).await?;
    let departments = survey_audience_department::Entity::find()
        .filter(survey_audience_department::Column::TenantId.eq(tenant))
        .filter(survey_audience_department::Column::SurveyId.eq(survey)).all(db).await?;
    let extra = survey_targeting::load_extensions(db, tenant, survey).await?;
    let scope = survey_audience_scope::Entity::find_by_id(survey).filter(survey_audience_scope::Column::TenantId.eq(tenant)).one(db).await?;
    let audience_kind = audience_kind(scope.map(|row| row.audience_kind), !departments.is_empty(), &extra)?;
    let source = survey_targeting::load_revision_source(db, tenant, survey).await?;
    Ok(SurveyAudience {
        audience_kind,
        department_ids: departments.into_iter().map(|r| r.department_id.to_string().into()).collect(),
        location_ids: extra.location_ids.into_iter().map(|id| id.to_string().into()).collect(),
        employee_ids: extra.employee_ids.into_iter().map(|id| id.to_string().into()).collect(),
        source_survey_id: source.map(|id| id.to_string().into()),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn administration_preserves_restricted_scope_when_targets_are_deleted() {
        let empty = survey_targeting::AudienceExtensions::default();
        for kind in ["DEPARTMENT", "LOCATION", "EMPLOYEE", "ALL"] {
            assert_eq!(audience_kind(Some(kind.into()), false, &empty).unwrap(), kind);
        }
        assert!(audience_kind(None, false, &empty).is_err());
        assert!(audience_kind(Some("UNKNOWN".into()), false, &empty).is_err());
        assert_eq!(audience_kind(None, true, &empty).unwrap(), "DEPARTMENT");
    }
}

pub async fn options(db: &DatabaseConnection, tenant: Uuid, kind: &str, search: Option<String>, after: Option<Uuid>, limit: i32) -> KabiPayResult<SurveyAudienceOptions> {
    if !(1..=100).contains(&limit) { return Err(KabiPayError::Validation("Audience page size must be between 1 and 100".into())); }
    let search = search.unwrap_or_default();
    let search = search.trim();
    if search.chars().count() > 100 { return Err(KabiPayError::Validation("Audience search is too long".into())); }
    let mut nodes: Vec<SurveyAudienceOption> = match kind {
        "DEPARTMENT" => {
            let mut query = department::Entity::find().filter(department::Column::TenantId.eq(tenant))
                .filter(department::Column::IsDeleted.eq(false));
            if let Some(id) = after { query = query.filter(department::Column::Id.gt(id)); }
            if !search.is_empty() { query = query.filter(department::Column::Name.contains(search)); }
            query.order_by_asc(department::Column::Id).limit(limit as u64 + 1).all(db).await?
                .into_iter().map(|r| SurveyAudienceOption { id: r.id.to_string().into(), label: r.name }).collect()
        }
        "LOCATION" => {
            let mut query = location::Entity::find().filter(location::Column::TenantId.eq(tenant))
                .filter(location::Column::IsDeleted.eq(false));
            if let Some(id) = after { query = query.filter(location::Column::Id.gt(id)); }
            if !search.is_empty() { query = query.filter(location::Column::Name.contains(search)); }
            query.order_by_asc(location::Column::Id).limit(limit as u64 + 1).all(db).await?
                .into_iter().map(|r| SurveyAudienceOption { id: r.id.to_string().into(), label: r.name }).collect()
        }
        "EMPLOYEE" => {
            let mut query = employee::Entity::find().filter(employee::Column::TenantId.eq(tenant))
                .filter(employee::Column::IsDeleted.eq(false)).filter(employee::Column::UserId.is_not_null())
                .filter(employee::Column::Status.is_in(["ACTIVE", "PROBATION", "ON_LEAVE"]));
            if let Some(id) = after { query = query.filter(employee::Column::Id.gt(id)); }
            if !search.is_empty() { query = query.filter(Condition::any().add(employee::Column::FirstName.contains(search)).add(employee::Column::LastName.contains(search))); }
            query.order_by_asc(employee::Column::Id).limit(limit as u64 + 1).all(db).await?
                .into_iter().map(|r| SurveyAudienceOption { id: r.id.to_string().into(), label: format!("{} {}", r.first_name, r.last_name) }).collect()
        }
        _ => return Err(KabiPayError::Validation("Unknown audience type".into())),
    };
    let next_cursor = if nodes.len() > limit as usize {
        nodes.truncate(limit as usize);
        nodes.last().map(|r| r.id.clone())
    } else { None };
    Ok(SurveyAudienceOptions { nodes, next_cursor })
}
