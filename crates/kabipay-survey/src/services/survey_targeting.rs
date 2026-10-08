//! Survey audience validation and immutable correction lineage. Call writes in the survey transaction.
use std::collections::HashSet;
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0006_org_hierarchy::{department, location}, d0007_employee_core::employee,
    d0076_anonymous_surveys::survey,
    d0084_survey_targeting_corrections::{survey_audience_employee, survey_audience_location, survey_audience_scope, survey_revision},
};
use sea_orm::{ActiveModelTrait, ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter, QuerySelect, Set};
use uuid::Uuid;

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct AudienceExtensions { pub location_ids: Vec<Uuid>, pub employee_ids: Vec<Uuid> }

fn validate_selection(departments: &[Uuid], audience: &AudienceExtensions) -> KabiPayResult<()> {
    let groups = [departments, audience.location_ids.as_slice(), audience.employee_ids.as_slice()];
    if groups.iter().filter(|ids| !ids.is_empty()).count() > 1 {
        return Err(KabiPayError::Validation("Select only one audience type: departments, locations, or employees".into()));
    }
    if groups.iter().any(|ids| ids.iter().collect::<HashSet<_>>().len() != ids.len()) {
        return Err(KabiPayError::Validation("Audience IDs must be unique".into()));
    }
    Ok(())
}

fn eligible_employees(tenant_id: Uuid) -> sea_orm::Select<employee::Entity> {
    employee::Entity::find().filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false)).filter(employee::Column::UserId.is_not_null())
        .filter(employee::Column::Status.is_in(["ACTIVE", "PROBATION", "ON_LEAVE"]))
}

fn selected_kind(department_ids: &[Uuid], audience: &AudienceExtensions) -> &'static str {
    if !department_ids.is_empty() { "DEPARTMENT" }
    else if !audience.location_ids.is_empty() { "LOCATION" }
    else if !audience.employee_ids.is_empty() { "EMPLOYEE" }
    else { "ALL" }
}

fn validate_saved_scope(kind: Option<&str>, department_ids: &[Uuid], audience: &AudienceExtensions) -> KabiPayResult<()> {
    validate_selection(department_ids, audience)?;
    let actual = selected_kind(department_ids, audience);
    // Legacy department drafts retain their explicit restriction. Missing or erased
    // targeting must never silently expand into the whole employee population.
    if kind == Some(actual) || (kind.is_none() && actual == "DEPARTMENT") { return Ok(()); }
    Err(KabiPayError::Validation("Survey audience changed or is incomplete; review and save the draft audience again".into()))
}

pub async fn save_scope<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid, department_ids: &[Uuid], audience: &AudienceExtensions) -> KabiPayResult<()> {
    validate_selection(department_ids, audience)?;
    survey_audience_scope::Entity::delete_many().filter(survey_audience_scope::Column::TenantId.eq(tenant_id)).filter(survey_audience_scope::Column::SurveyId.eq(survey_id)).exec(db).await?;
    survey_audience_scope::ActiveModel { tenant_id: Set(tenant_id), survey_id: Set(survey_id), audience_kind: Set(selected_kind(department_ids, audience).into()) }.insert(db).await?;
    Ok(())
}

pub async fn validate_audience<C: ConnectionTrait>(db: &C, tenant_id: Uuid, department_ids: &[Uuid], audience: &AudienceExtensions) -> KabiPayResult<()> {
    validate_selection(department_ids, audience)?;
    if !department_ids.is_empty() && department::Entity::find().filter(department::Column::TenantId.eq(tenant_id))
        .filter(department::Column::IsDeleted.eq(false)).filter(department::Column::Id.is_in(department_ids.iter().copied())).all(db).await?.len() != department_ids.len() {
        return Err(KabiPayError::Validation("Every audience department must be active and belong to this tenant".into()));
    }
    if !audience.location_ids.is_empty() && location::Entity::find().filter(location::Column::TenantId.eq(tenant_id))
        .filter(location::Column::IsDeleted.eq(false)).filter(location::Column::Id.is_in(audience.location_ids.iter().copied())).all(db).await?.len() != audience.location_ids.len() {
        return Err(KabiPayError::Validation("Every audience location must be active and belong to this tenant".into()));
    }
    if !audience.employee_ids.is_empty() && eligible_employees(tenant_id).filter(employee::Column::Id.is_in(audience.employee_ids.iter().copied())).all(db).await?.len() != audience.employee_ids.len() {
        return Err(KabiPayError::Validation("Every selected employee must have an active eligible account in this tenant".into()));
    }
    Ok(())
}

pub async fn save_extensions<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid, audience: &AudienceExtensions) -> KabiPayResult<()> {
    survey_audience_location::Entity::delete_many().filter(survey_audience_location::Column::TenantId.eq(tenant_id)).filter(survey_audience_location::Column::SurveyId.eq(survey_id)).exec(db).await?;
    survey_audience_employee::Entity::delete_many().filter(survey_audience_employee::Column::TenantId.eq(tenant_id)).filter(survey_audience_employee::Column::SurveyId.eq(survey_id)).exec(db).await?;
    for id in &audience.location_ids {
        survey_audience_location::ActiveModel { tenant_id: Set(tenant_id), survey_id: Set(survey_id), location_id: Set(*id) }.insert(db).await?;
    }
    for id in &audience.employee_ids {
        survey_audience_employee::ActiveModel { tenant_id: Set(tenant_id), survey_id: Set(survey_id), employee_id: Set(*id) }.insert(db).await?;
    }
    Ok(())
}

pub async fn load_extensions<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid) -> KabiPayResult<AudienceExtensions> {
    let locations = survey_audience_location::Entity::find().filter(survey_audience_location::Column::TenantId.eq(tenant_id)).filter(survey_audience_location::Column::SurveyId.eq(survey_id)).all(db).await?;
    let employees = survey_audience_employee::Entity::find().filter(survey_audience_employee::Column::TenantId.eq(tenant_id)).filter(survey_audience_employee::Column::SurveyId.eq(survey_id)).all(db).await?;
    Ok(AudienceExtensions { location_ids: locations.into_iter().map(|row| row.location_id).collect(), employee_ids: employees.into_iter().map(|row| row.employee_id).collect() })
}

pub async fn publication_employees<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid, department_ids: &[Uuid]) -> KabiPayResult<Vec<employee::Model>> {
    let audience = load_extensions(db, tenant_id, survey_id).await?;
    let scope = survey_audience_scope::Entity::find_by_id(survey_id).filter(survey_audience_scope::Column::TenantId.eq(tenant_id)).one(db).await?;
    validate_saved_scope(scope.as_ref().map(|row| row.audience_kind.as_str()), department_ids, &audience)?;
    validate_audience(db, tenant_id, department_ids, &audience).await?;
    let mut query = eligible_employees(tenant_id);
    if !department_ids.is_empty() { query = query.filter(employee::Column::DepartmentId.is_in(department_ids.iter().copied())); }
    if !audience.location_ids.is_empty() { query = query.filter(employee::Column::LocationId.is_in(audience.location_ids)); }
    if !audience.employee_ids.is_empty() { query = query.filter(employee::Column::Id.is_in(audience.employee_ids)); }
    query.all(db).await.map_err(KabiPayError::from)
}

/// Source must already be immutable; call only when inserting a new draft.
pub async fn save_revision<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid, source_survey_id: Uuid) -> KabiPayResult<()> {
    if survey_id == source_survey_id { return Err(KabiPayError::Validation("A correction must be a new survey".into())); }
    let source = survey::Entity::find_by_id(source_survey_id).filter(survey::Column::TenantId.eq(tenant_id)).lock_shared().one(db).await?
        .ok_or_else(|| KabiPayError::Validation("Correction source must belong to this tenant".into()))?;
    if !matches!(source.status.as_str(), "PUBLISHED" | "CLOSED") { return Err(KabiPayError::Validation("Corrections require a published or closed source survey".into())); }
    survey_revision::ActiveModel { tenant_id: Set(tenant_id), survey_id: Set(survey_id), source_survey_id: Set(source_survey_id) }.insert(db).await?;
    Ok(())
}

pub async fn load_revision_source<C: ConnectionTrait>(db: &C, tenant_id: Uuid, survey_id: Uuid) -> KabiPayResult<Option<Uuid>> {
    Ok(survey_revision::Entity::find_by_id(survey_id).filter(survey_revision::Column::TenantId.eq(tenant_id)).one(db).await?.map(|row| row.source_survey_id))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn erased_targets_never_become_all_employees() {
        let empty = AudienceExtensions::default();
        for kind in [None, Some("DEPARTMENT"), Some("LOCATION"), Some("EMPLOYEE"), Some("UNKNOWN")] {
            assert!(validate_saved_scope(kind, &[], &empty).is_err());
        }
        assert!(validate_saved_scope(Some("ALL"), &[], &empty).is_ok());
        assert!(validate_saved_scope(None, &[Uuid::new_v4()], &empty).is_ok());
        assert!(validate_saved_scope(Some("ALL"), &[Uuid::new_v4()], &empty).is_err());
    }
    use sea_orm::{DbBackend, QueryTrait};
    #[test]
    fn publication_query_requires_tenant_and_eligible_linked_employee() {
        let tenant_id = Uuid::new_v4();
        let sql = eligible_employees(tenant_id).build(DbBackend::Postgres).to_string();
        assert!(sql.contains(&tenant_id.to_string()));
        for predicate in ["\"is_deleted\" = FALSE", "\"user_id\" IS NOT NULL", "'ACTIVE'", "'PROBATION'", "'ON_LEAVE'"] {
            assert!(sql.contains(predicate), "Missing {predicate}: {sql}");
        }
    }
    #[test]
    fn audience_modes_are_exclusive_and_unique() {
        let id = Uuid::new_v4();
        assert!(validate_selection(&[], &AudienceExtensions::default()).is_ok());
        assert!(validate_selection(&[id], &AudienceExtensions::default()).is_ok());
        assert!(validate_selection(&[id], &AudienceExtensions { location_ids: vec![id], employee_ids: vec![] }).is_err());
        assert!(validate_selection(&[], &AudienceExtensions { location_ids: vec![id], employee_ids: vec![id] }).is_err());
        assert!(validate_selection(&[], &AudienceExtensions { location_ids: vec![], employee_ids: vec![id, id] }).is_err());
    }
}
