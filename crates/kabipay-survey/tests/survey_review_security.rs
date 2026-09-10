use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use chrono::Utc;
use sea_orm::{entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
use uuid::Uuid;
use kabipay_survey::services::survey_review::submissions;

#[derive(Clone, Debug)]
struct ReviewFixture {
    tenant: Uuid, survey: Uuid, response: Uuid, question: Uuid, mode: &'static str, status: &'static str,
    queries: Arc<Mutex<Vec<String>>>,
}
impl ReviewFixture {
    fn new() -> Self { Self { tenant: Uuid::new_v4(), survey: Uuid::new_v4(), response: Uuid::new_v4(), question: Uuid::new_v4(), mode: "ANONYMOUS_SUBMISSIONS", status: "CLOSED", queries: Arc::default() } }
    async fn db(&self) -> DatabaseConnection { Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(self.clone()))).await.unwrap() }
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for ReviewFixture {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.queries.lock().unwrap().push(sql.clone());
        assert!(sql.contains(&self.tenant.to_string()), "Missing tenant constraint: {sql}");
        if sql.contains("FROM \"survey_response\"") {
            assert!(sql.contains(&self.survey.to_string()));
            if sql.contains("COUNT") { return Ok(vec![ProxyRow::new(BTreeMap::from([("num_items".into(), 3i64.into())]))]); }
            assert!(sql.contains("ORDER BY \"survey_response\".\"id\" ASC"), "{sql}");
            assert!(sql.contains("LIMIT 1 OFFSET 1"), "{sql}");
            assert!(!sql.contains("department_id") && !sql.contains("manager_employee_id") && !sql.contains("location_id"));
            return Ok(vec![ProxyRow::new(BTreeMap::from([("id".into(), self.response.into())]))]);
        }
        if sql.contains("FROM \"survey\"") {
            assert!(sql.contains(&self.survey.to_string()));
            let now = Utc::now();
            return Ok(vec![ProxyRow::new(BTreeMap::from([
                ("id".into(), self.survey.into()), ("tenant_id".into(), self.tenant.into()),
                ("title".into(), "Pulse".into()), ("description".into(), Option::<String>::None.into()),
                ("status".into(), self.status.into()), ("response_review_mode".into(), self.mode.into()),
                ("opens_at".into(), Option::<chrono::DateTime<Utc>>::None.into()), ("closes_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
                ("minimum_report_group_size".into(), 3i32.into()), ("created_by".into(), Uuid::new_v4().into()),
                ("published_at".into(), Some(now).into()), ("closed_at".into(), Some(now).into()),
                ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
            ]))]);
        }
        if sql.contains("FROM \"survey_section\"") { return Ok(vec![ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::nil().into()), ("tenant_id".into(), self.tenant.into()), ("survey_id".into(), self.survey.into()), ("title".into(), "Section".into()), ("display_order".into(), 0i32.into()),
        ]))]); }
        if sql.contains("FROM \"survey_question\"") { return Ok(vec![ProxyRow::new(BTreeMap::from([
            ("id".into(), self.question.into()), ("tenant_id".into(), self.tenant.into()), ("section_id".into(), Uuid::nil().into()),
            ("dimension".into(), "Wellbeing".into()), ("question_type".into(), "RATING".into()), ("prompt".into(), "How was your week?".into()),
            ("description".into(), Some("Think about this week".to_string()).into()), ("comment_enabled".into(), true.into()), ("is_required".into(), false.into()),
            ("rating_min".into(), Some(rust_decimal::Decimal::ONE).into()), ("rating_max".into(), Some(rust_decimal::Decimal::from(5)).into()), ("display_order".into(), 0i32.into()),
        ]))]); }
        if sql.contains("FROM \"survey_answer\"") {
            assert!(sql.contains(&self.response.to_string()));
            return Ok(vec![ProxyRow::new(BTreeMap::from([
                ("id".into(), Uuid::new_v4().into()), ("tenant_id".into(), self.tenant.into()), ("survey_response_id".into(), self.response.into()), ("question_id".into(), self.question.into()),
                ("numeric_answer".into(), Some(rust_decimal::Decimal::from(4)).into()), ("text_answer".into(), Option::<String>::None.into()),
                ("comment".into(), Some("Useful feedback".to_owned()).into()), ("selected_option_ids".into(), Option::<serde_json::Value>::None.into()),
            ]))]);
        }
        Ok(vec![])
    }
    async fn execute(&self, _: Statement) -> Result<ProxyExecResult, DbErr> { panic!("Review must never write"); }
}

#[tokio::test]
async fn paged_review_scopes_every_read_and_bundles_rating_with_comment_without_response_id() {
    let fixture = ReviewFixture::new();
    let page = submissions(&fixture.db().await, fixture.tenant, fixture.survey, 1, 1).await.unwrap();
    assert!(page.available && page.has_more);
    assert_eq!(page.total_count, Some(3));
    assert_eq!(page.nodes[0].number, 2);
    assert_eq!(page.nodes[0].answers[0].numeric_answer.as_deref(), Some("4"));
    assert_eq!(page.nodes[0].answers[0].comment.as_deref(), Some("Useful feedback"));
    assert_eq!(page.nodes[0].answers[0].question_id.as_str(), fixture.question.to_string());
}

#[tokio::test]
async fn legacy_and_open_surveys_do_not_load_any_response_rows() {
    for (mode, status) in [("AGGREGATE_ONLY", "CLOSED"), ("ANONYMOUS_SUBMISSIONS", "PUBLISHED")] {
        let mut fixture = ReviewFixture::new(); fixture.mode = mode; fixture.status = status;
        let page = submissions(&fixture.db().await, fixture.tenant, fixture.survey, 0, 20).await.unwrap();
        assert!(!page.available); assert!(page.total_count.is_none()); assert!(page.nodes.is_empty());
        assert_eq!(fixture.queries.lock().unwrap().len(), 1);
    }
}

#[tokio::test]
async fn invalid_paging_is_rejected_before_reading_survey() {
    let fixture = ReviewFixture::new();
    for (offset, limit) in [(-1, 20), (0, 0), (0, 101)] {
        assert!(submissions(&fixture.db().await, fixture.tenant, fixture.survey, offset, limit).await.is_err());
    }
    assert!(fixture.queries.lock().unwrap().is_empty());
}

#[tokio::test]
async fn review_mode_blocks_live_aggregates_and_organizational_slices_before_response_reads() {
    use kabipay_survey::services::survey_service::{aggregate_results, ResultScope};
    for (status, scope) in [("PUBLISHED", ResultScope::All), ("CLOSED", ResultScope::Department(Uuid::new_v4())), ("CLOSED", ResultScope::Team(Uuid::new_v4()))] {
        let mut fixture = ReviewFixture::new(); fixture.status = status;
        let result = aggregate_results(&fixture.db().await, fixture.tenant, fixture.survey, scope, true).await.unwrap();
        assert!(result.suppressed);
        assert!(result.respondent_count.is_none() && result.questions.is_empty());
        assert!(result.suppression_reason.is_some());
        assert_eq!(fixture.queries.lock().unwrap().len(), 1);
    }
}
