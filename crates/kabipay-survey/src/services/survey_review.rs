//! Closed-survey review exposes only questionnaire content, never participation metadata.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0076_anonymous_surveys::{survey_assignment, survey_response, survey_answer};
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter, QueryOrder, QuerySelect, PaginatorTrait};
use uuid::Uuid;
use crate::resolvers::types::{SurveySummaryDto, SurveySubmissions, SurveyAnonymousSubmission, SurveySubmissionAnswer};
use super::survey_service;

pub fn review_available(mode: &str, status: &str) -> bool {
    mode == "ANONYMOUS_SUBMISSIONS" && status == "CLOSED"
}

/// Caller must enforce survey:manage ALL. Counts contain no assignment identities.
pub async fn populate_counts(db: &DatabaseConnection, tenant: Uuid, summary: &mut SurveySummaryDto) -> KabiPayResult<()> {
    populate_list_counts(db, tenant, std::slice::from_mut(summary)).await
}

/// One grouped query for the complete management catalog; never loads employee IDs.
pub async fn populate_list_counts(db: &DatabaseConnection, tenant: Uuid, summaries: &mut [SurveySummaryDto]) -> KabiPayResult<()> {
    if summaries.is_empty() { return Ok(()); }
    let ids: Vec<Uuid> = summaries.iter().map(|summary| Uuid::parse_str(summary.id.as_str())
        .map_err(|_| KabiPayError::Validation("Invalid survey ID".into()))).collect::<KabiPayResult<_>>()?;
    let query = survey_assignment::Entity::find().filter(survey_assignment::Column::TenantId.eq(tenant))
        .filter(survey_assignment::Column::SurveyId.is_in(ids));
    let counts = query.select_only().column(survey_assignment::Column::SurveyId).column(survey_assignment::Column::Completed)
        .column_as(sea_orm::sea_query::Expr::col(survey_assignment::Column::Id).count(), "count")
        .group_by(survey_assignment::Column::SurveyId).group_by(survey_assignment::Column::Completed)
        .into_tuple::<(Uuid, bool, i64)>().all(db).await?;
    let mut by_survey = std::collections::HashMap::<String, (i64, i64)>::new();
    for (id, done, count) in counts {
        let entry = by_survey.entry(id.to_string()).or_default();
        if done { entry.0 = count; } else { entry.1 = count; }
    }
    let checked = |count| i32::try_from(count).map_err(|_| KabiPayError::Validation("Survey count exceeds supported range".into()));
    for summary in summaries {
        let (completed, pending) = by_survey.get(summary.id.as_str()).copied().unwrap_or_default();
        summary.assigned_count = Some(checked(completed + pending)?);
        summary.completed_count = Some(checked(completed)?);
        summary.pending_count = Some(checked(pending)?);
    }
    Ok(())
}

/// Caller must enforce both survey:manage ALL and survey:results ALL.
pub async fn submissions(db: &DatabaseConnection, tenant: Uuid, survey_id: Uuid, offset: i32, limit: i32) -> KabiPayResult<SurveySubmissions> {
    if offset < 0 || !(1..=100).contains(&limit) {
        return Err(KabiPayError::Validation("Submission offset must be nonnegative and page size between 1 and 100".into()));
    }
    let survey = survey_service::load_survey_model(db, tenant, survey_id).await?;
    if !review_available(&survey.response_review_mode, &survey.status) {
        return Ok(SurveySubmissions { available: false, reason: Some(if survey.response_review_mode != "ANONYMOUS_SUBMISSIONS" {
            "This survey allows aggregate results only".into()
        } else { "Individual submissions become available after this survey is closed".into() }), total_count: None, nodes: vec![], has_more: false });
    }
    let query = survey_response::Entity::find().filter(survey_response::Column::TenantId.eq(tenant))
        .filter(survey_response::Column::SurveyId.eq(survey_id));
    let total = query.clone().count(db).await?;
    // Random UUID v4 ordering is independent of submission time. IDs never leave this service.
    let responses = query.order_by_asc(survey_response::Column::Id).offset(offset as u64).limit(limit as u64)
        .select_only().column(survey_response::Column::Id).into_tuple::<Uuid>().all(db).await?;
    let definition = survey_service::load_survey(db, tenant, survey_id, false).await?;
    let answers = if responses.is_empty() { vec![] } else {
        survey_answer::Entity::find().filter(survey_answer::Column::TenantId.eq(tenant))
            .filter(survey_answer::Column::SurveyResponseId.is_in(responses.iter().copied())).all(db).await?
    };
    let nodes = responses.iter().enumerate().map(|(index, response)| {
        let mut values = Vec::new();
        for question in definition.sections.iter().flat_map(|section| &section.questions) {
            let answer = answers.iter().find(|answer| answer.survey_response_id == *response && answer.question_id.to_string() == question.id.as_str());
            let selected = answer.and_then(|answer| answer.selected_option_ids.as_ref()).and_then(serde_json::Value::as_array);
            values.push(SurveySubmissionAnswer {
                question_id: question.id.clone(), prompt: question.prompt.clone(),
                numeric_answer: answer.and_then(|answer| answer.numeric_answer).map(|value| value.to_string()),
                text_answer: answer.and_then(|answer| answer.text_answer.clone()),
                comment: answer.and_then(|answer| answer.comment.clone()),
                selected_options: question.options.iter().filter(|option| selected.is_some_and(|ids| ids.iter().any(|id| id.as_str() == Some(option.id.as_str()))))
                    .map(|option| option.label.clone()).collect(),
            });
        }
        SurveyAnonymousSubmission { number: offset.saturating_add(index as i32).saturating_add(1), answers: values }
    }).collect();
    Ok(SurveySubmissions { available: true, reason: None,
        total_count: Some(i32::try_from(total).map_err(|_| KabiPayError::Validation("Survey count exceeds supported range".into()))?),
        has_more: offset as u64 + (responses.len() as u64) < total, nodes })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn review_requires_explicit_mode_and_permanent_closure() {
        for status in ["DRAFT", "PUBLISHED", "CLOSED"] {
            assert!(!review_available("AGGREGATE_ONLY", status));
        }
        assert!(!review_available("ANONYMOUS_SUBMISSIONS", "PUBLISHED"));
        assert!(!review_available("ANONYMOUS_SUBMISSIONS", "DRAFT"));
        assert!(review_available("ANONYMOUS_SUBMISSIONS", "CLOSED"));
    }

    #[tokio::test]
    async fn individual_review_requires_both_exact_all_scopes_before_database_access() {
        for (manage, results) in [("ALL", "TEAM"), ("ALL", "DEPARTMENT"), ("TEAM", "ALL"), ("SELF", "ALL"), ("ALL", "SELF")] {
            let claims: kabipay_common::context::ClientClaims = serde_json::from_value(serde_json::json!({
                "sub": Uuid::new_v4(), "tenant_id": Uuid::new_v4(), "iss": "kabipay-client",
                "iat": 0, "exp": 9999999999i64, "permissions": ["survey:manage", "survey:results"],
                "permission_scopes": {"survey:manage": manage, "survey:results": results}
            })).unwrap();
            let schema = async_graphql::Schema::build(crate::resolvers::QueryRoot, crate::resolvers::MutationRoot, async_graphql::EmptySubscription).data(claims).finish();
            let result = schema.execute(format!("{{ surveySubmissions(surveyId: \"{}\") {{ available totalCount nodes {{ number answers {{ questionId prompt comment }} }} }} }}", Uuid::new_v4())).await;
            assert_eq!(result.errors.len(), 1);
            assert!(result.errors[0].message.contains("survey:manage and survey:results with ALL scope required"));
        }
    }

    #[test]
    fn submission_schema_excludes_identity_time_and_organization() {
        let schema = async_graphql::Schema::build(crate::resolvers::QueryRoot, crate::resolvers::MutationRoot, async_graphql::EmptySubscription).finish().sdl();
        for name in ["SurveyAnonymousSubmission", "SurveySubmissionAnswer", "SurveySubmissions"] {
            let block = schema.split(&format!("type {name} {{")).nth(1).unwrap().split('}').next().unwrap();
            for forbidden in ["employee", "respondent", "responseId", "submittedAt", "createdAt", "department", "location", "manager", "actor"] {
                assert!(!block.contains(forbidden), "{name} exposes {forbidden}");
            }
        }
    }
}
