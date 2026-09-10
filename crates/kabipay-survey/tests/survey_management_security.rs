use std::{collections::BTreeMap, sync::{Arc, Mutex}};
use async_graphql::{EmptySubscription, Schema};
use chrono::Utc;
use kabipay_common::context::ClientClaims;
use kabipay_survey::{resolvers::{MutationRoot, QueryRoot}, services::{survey_management, survey_targeting::{self, AudienceExtensions}}};
use sea_orm::{entity::prelude::async_trait, Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, Statement};
use uuid::Uuid;

#[derive(Clone, Debug)]
struct Fixture {
    tenant: Uuid,
    survey: Uuid,
    scope: Option<String>,
    department_rows: usize,
    fail_queries: bool,
    queries: Arc<Mutex<Vec<String>>>,
}
impl Fixture {
    fn new() -> Self { Self { tenant: Uuid::new_v4(), survey: Uuid::new_v4(), scope: None, department_rows: 0, fail_queries: false, queries: Arc::new(Mutex::new(vec![])) } }
    async fn db(&self) -> DatabaseConnection { Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(self.clone()))).await.unwrap() }
    fn queries(&self) -> Vec<String> { self.queries.lock().unwrap().clone() }
    fn department(&self, index: usize) -> ProxyRow {
        let now = Utc::now();
        ProxyRow::new(BTreeMap::from([
            ("id".into(), Uuid::from_u128(index as u128 + 1).into()),
            ("tenant_id".into(), self.tenant.into()),
            ("parent_department_id".into(), Option::<Uuid>::None.into()),
            ("name".into(), format!("Department {index}").into()),
            ("code".into(), format!("D{index}").into()),
            ("head_employee_id".into(), Option::<Uuid>::None.into()),
            ("is_deleted".into(), false.into()),
            ("deleted_at".into(), Option::<chrono::DateTime<Utc>>::None.into()),
            ("deleted_by".into(), Option::<Uuid>::None.into()),
            ("created_at".into(), now.into()), ("updated_at".into(), now.into()),
        ]))
    }
}
#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.queries.lock().unwrap().push(sql.clone());
        if self.fail_queries { return Err(DbErr::Custom("targeting database unavailable".into())); }
        if sql.contains("FROM \"survey_audience_scope\"") {
            return Ok(self.scope.as_ref().map(|kind| ProxyRow::new(BTreeMap::from([
                ("tenant_id".into(), self.tenant.into()), ("survey_id".into(), self.survey.into()),
                ("audience_kind".into(), kind.clone().into()),
            ]))).into_iter().collect());
        }
        if sql.contains("FROM \"department\"") { return Ok((0..self.department_rows).map(|i| self.department(i)).collect()); }
        Ok(vec![])
    }
    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        Err(DbErr::Custom(format!("Read-only security fixture rejects writes: {statement}")))
    }
}

#[tokio::test]
async fn audience_identity_apis_reject_respondents_results_viewers_and_narrow_managers_before_database_access() {
    for (permission, scope) in [("survey:respond", "SELF"), ("survey:results", "ALL"), ("survey:manage", "TEAM"), ("survey:manage", "DEPARTMENT"), ("survey:manage", "SELF")] {
        let claims: ClientClaims = serde_json::from_value(serde_json::json!({
            "sub": Uuid::new_v4(), "tenant_id": Uuid::new_v4(), "iss": "kabipay-client", "iat": 0, "exp": 9999999999i64,
            "permissions": [permission], "permission_scopes": { (permission): scope }
        })).unwrap();
        let schema = Schema::build(QueryRoot, MutationRoot, EmptySubscription).data(claims).finish();
        for query in ["{ surveyAudience(surveyId: \"00000000-0000-0000-0000-000000000001\") { employeeIds } }", "{ surveyAudienceOptions(kind: \"EMPLOYEE\") { nodes { id label } } }"] {
            let result = schema.execute(query).await;
            assert_eq!(result.errors.len(), 1, "{permission}/{scope}: {:?}", result.errors);
            assert!(result.errors[0].message.contains("ALL scope required"), "{:?}", result.errors);
        }
    }
}

#[tokio::test]
async fn audience_page_uses_last_returned_cursor_and_bounded_tenant_query() {
    let mut fixture = Fixture::new();
    fixture.department_rows = 3;
    let page = survey_management::options(&fixture.db().await, fixture.tenant, "DEPARTMENT", Some("Operations".into()), Some(Uuid::nil()), 2).await.unwrap();
    assert_eq!(page.nodes.len(), 2);
    assert_eq!(page.next_cursor.as_ref().map(|id| id.as_str()), Some(Uuid::from_u128(2).to_string().as_str()));
    let queries = fixture.queries();
    assert_eq!(queries.len(), 1);
    for required in [fixture.tenant.to_string(), "\"is_deleted\" = FALSE".into(), "\"id\" >".into(), "ORDER BY \"department\".\"id\" ASC".into(), "LIMIT 3".into(), "Operations".into()] {
        assert!(queries[0].contains(&required), "Missing {required}: {}", queries[0]);
    }
}

#[tokio::test]
async fn final_page_has_no_next_cursor_and_bad_limits_do_not_query() {
    let mut fixture = Fixture::new();
    fixture.department_rows = 2;
    let db = fixture.db().await;
    let page = survey_management::options(&db, fixture.tenant, "DEPARTMENT", None, None, 2).await.unwrap();
    assert!(page.next_cursor.is_none());
    let count = fixture.queries().len();
    for limit in [0, -1, 101] { assert!(survey_management::options(&db, fixture.tenant, "EMPLOYEE", None, None, limit).await.is_err()); }
    assert!(survey_management::options(&db, fixture.tenant, "INVALID", None, None, 2).await.is_err());
    assert!(survey_management::options(&db, fixture.tenant, "EMPLOYEE", Some("x".repeat(101)), None, 2).await.is_err());
    assert_eq!(fixture.queries().len(), count);
}

#[tokio::test]
async fn missing_or_erased_scope_never_reaches_employee_population_query() {
    for kind in [None, Some("DEPARTMENT"), Some("LOCATION"), Some("EMPLOYEE")] {
        let mut fixture = Fixture::new(); fixture.scope = kind.map(str::to_owned);
        let result = survey_targeting::publication_employees(&fixture.db().await, fixture.tenant, fixture.survey, &[]).await;
        assert!(result.is_err());
        assert!(fixture.queries().iter().all(|q| !q.contains("FROM \"employee\"")));
    }
    let mut fixture = Fixture::new(); fixture.scope = Some("ALL".into());
    assert!(survey_targeting::publication_employees(&fixture.db().await, fixture.tenant, fixture.survey, &[]).await.is_ok());
    assert!(fixture.queries().iter().any(|q| q.contains("FROM \"employee\"")));
}

#[tokio::test]
async fn invalid_targets_and_database_failure_never_become_empty_all_audience() {
    for audience in [AudienceExtensions { location_ids: vec![Uuid::new_v4()], employee_ids: vec![] }, AudienceExtensions { location_ids: vec![], employee_ids: vec![Uuid::new_v4()] }] {
        let fixture = Fixture::new();
        assert!(survey_targeting::validate_audience(&fixture.db().await, fixture.tenant, &[], &audience).await.is_err());
        assert!(fixture.queries().iter().all(|q| q.contains(&fixture.tenant.to_string())));
    }
    let mut fixture = Fixture::new(); fixture.fail_queries = true;
    let result = survey_targeting::publication_employees(&fixture.db().await, fixture.tenant, fixture.survey, &[]).await;
    assert!(result.is_err());
    assert_eq!(fixture.queries().len(), 1);
}
