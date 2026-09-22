//! Resolver SQL-boundary tests; these do not simulate PostgreSQL lock scheduling.

use std::{collections::{BTreeMap, HashMap}, env, sync::{Arc, Mutex}, time::{Duration, Instant}};
use async_graphql::{Context, EmptySubscription, Schema};
use chrono::Utc;
use kabipay_common::{context::ClientClaims, subgraph::TenantId};
use kabipay_db_entities::tenant::{d0018_performance::goal, d0075_performance_appraisal_lifecycle::{appraisal_template, appraisal_template_section, appraisal_question, performance_participant, performance_program}};
use rust_decimal::Decimal;
use sea_orm::{entity::prelude::async_trait, sqlx::error::Error as SqlxError, ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, DbErr, EntityTrait, Iden, Iterable, ModelTrait, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, RuntimeErr, Statement, TransactionTrait};
use uuid::Uuid;
use super::{MutationRoot, QueryRoot};

struct TestTenantDb(Uuid, DatabaseConnection);

pub(super) async fn tenant_db(ctx: &Context<'_>, tenant: Uuid) -> async_graphql::Result<DatabaseConnection> {
    if let Some(db) = ctx.data_opt::<TestTenantDb>() {
        assert_eq!(db.0, tenant, "resolver must retain request tenant");
        return Ok(db.1.clone());
    }
    kabipay_common::subgraph::tenant_db(ctx, tenant).await
}

fn row<M: ModelTrait>(model: M) -> ProxyRow {
    ProxyRow::new(<M::Entity as EntityTrait>::Column::iter()
        .map(|column| (column.to_string(), model.get(column))).collect())
}

#[derive(Clone, Debug)]
struct Fixture {
    tenant: Uuid,
    id: Uuid,
    participant_id: Uuid,
    employee_id: Uuid,
    cycle_id: Uuid,
    template_id: Uuid,
    events: Arc<Mutex<Vec<String>>>,
    stage_after_lock: &'static str,
    empty_definition: bool,
    fail_goal_write: bool,
    initial_goals_approved: bool,
    claim_employee_id: Option<Uuid>,
    permissions: Vec<String>,
    permission_scopes: HashMap<String, String>,
    manager_employee_id: Option<Uuid>,
    is_excluded: bool,
    acknowledgement: Arc<Mutex<Option<(chrono::DateTime<Utc>, Option<String>)>>>,
    missing_goal_weight: bool,
    dependent: Option<&'static str>,
    pending_acknowledgements: u64,
    include_acknowledgement: bool,
    participant_cycle_after_lock: Option<Uuid>,
}

impl Fixture {
    fn new() -> Self {
        Self {
            tenant: Uuid::new_v4(), id: Uuid::new_v4(), participant_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(), cycle_id: Uuid::new_v4(), template_id: Uuid::new_v4(),
            events: Arc::default(), stage_after_lock: "GOAL_SETTING", empty_definition: false,
            fail_goal_write: false, initial_goals_approved: false,
            claim_employee_id: Some(Uuid::new_v4()), permissions: vec!["performance:manage".into()],
            permission_scopes: HashMap::from([("performance:manage".into(), "ALL".into())]),
            manager_employee_id: None, is_excluded: false, acknowledgement: Arc::default(),
            missing_goal_weight: false, dependent: None,
            pending_acknowledgements: 0, include_acknowledgement: true, participant_cycle_after_lock: None,
        }
    }

    fn with_claims(mut self, permission: &str, scope: &str, employee_id: Option<Uuid>) -> Self {
        self.permissions = vec![permission.into()];
        self.permission_scopes = HashMap::from([(permission.into(), scope.into())]);
        self.claim_employee_id = employee_id;
        self
    }

    fn own_employee(mut self) -> Self {
        self.claim_employee_id = Some(self.employee_id);
        self
    }

    fn assigned_manager(mut self) -> Self {
        let manager = Uuid::new_v4();
        self.claim_employee_id = Some(manager);
        self.manager_employee_id = Some(manager);
        self.permissions = vec!["performance:evaluate".into()];
        self.permission_scopes = HashMap::from([("performance:evaluate".into(), "TEAM".into())]);
        self
    }

    fn acknowledged(self, comment: &str) -> Self {
        *self.acknowledgement.lock().unwrap() = Some((Utc::now(), Some(comment.into())));
        self
    }

    fn acknowledgement(&self) -> Option<(chrono::DateTime<Utc>, Option<String>)> {
        self.acknowledgement.lock().unwrap().clone()
    }

    fn participant_cycle(&self) -> Uuid {
        if self.events().iter().any(|event| event.contains("SELECT current_stage FROM review_cycle")) {
            self.participant_cycle_after_lock.unwrap_or(self.cycle_id)
        } else {
            self.cycle_id
        }
    }

    fn record(&self, event: impl Into<String>) { self.events.lock().unwrap().push(event.into()); }
    fn events(&self) -> Vec<String> { self.events.lock().unwrap().clone() }

    async fn run(&self, operation: &str) -> async_graphql::Response {
        let db = Database::connect_proxy(DbBackend::Postgres, Arc::new(Box::new(self.clone()))).await.unwrap();
        let claims = ClientClaims {
            sub: self.id, iss: "kabipay-client".into(), exp: 0, iat: 0, tenant_id: self.tenant,
            email: String::new(), employee_id: self.claim_employee_id, must_change_password: false,
            roles: vec![], permissions: self.permissions.clone(),
            permission_scopes: self.permission_scopes.clone(), resource_scopes: HashMap::new(),
        };
        Schema::build(QueryRoot::default(), MutationRoot::default(), EmptySubscription)
            .data(TestTenantDb(self.tenant, db)).data(TenantId(self.tenant)).data(claims)
            .finish().execute(operation
                .replace("$PARTICIPANT", &self.participant_id.to_string())
                .replace("$CYCLE", &self.cycle_id.to_string())
                .replace("$TEMPLATE", &self.template_id.to_string())
                .replace("$GOAL", &self.id.to_string())).await
    }

    fn template(&self, published: bool) -> ProxyRow {
        row(appraisal_template::Model { id: self.template_id, tenant_id: self.tenant, performance_program_id: self.id, version: 1, name: "Review".into(), status: if published { "PUBLISHED" } else { "DRAFT" }.into(), published_at: None, published_by: None, created_at: Utc::now(), updated_at: Utc::now() })
    }

    fn participant(&self) -> ProxyRow {
        let acknowledgement = self.acknowledgement();
        row(performance_participant::Model {
            id: self.participant_id, tenant_id: self.tenant, review_cycle_id: self.participant_cycle(), employee_id: self.employee_id,
            manager_employee_id: self.manager_employee_id.or(Some(self.id)), department_id: None, designation_id: None,
            work_location_id: None, appraisal_template_id: self.template_id, status: "GOAL_SETTING".into(),
            is_excluded: self.is_excluded, exclusion_reason: self.is_excluded.then(|| "excluded".into()), response_revision: 1, self_submitted_at: None,
            manager_submitted_at: None, acknowledged_at: acknowledgement.as_ref().map(|(at, _)| *at), acknowledgement_comment: acknowledgement.and_then(|(_, comment)| comment),
            final_rating: None, performance_band: None, manager_rating: None, manager_performance_band: None, calibration_provenance: None, created_at: Utc::now(), updated_at: Utc::now(),
        })
    }

    fn goal(&self, approved: bool) -> ProxyRow {
        row(goal::Model { id: self.id, tenant_id: self.tenant, employee_id: self.employee_id, review_cycle_id: self.cycle_id,
            parent_goal_id: None, title: "Deliver".into(), description: None, weightage: (!self.missing_goal_weight).then_some(Decimal::ONE_HUNDRED),
            status: if approved { "APPROVED" } else { "PROPOSED" }.into(), visibility: Some("EMPLOYEE_MANAGER".into()), created_at: Utc::now(), updated_at: Utc::now() })
    }

    fn review_summary(&self) -> ProxyRow {
        let acknowledgement = self.acknowledgement();
        let mut values: BTreeMap<String, sea_orm::Value> = BTreeMap::new();
        values.insert("id".into(), self.participant_id.into());
        values.insert("review_cycle_id".into(), self.cycle_id.into());
        values.insert("employee_id".into(), self.employee_id.into());
        values.insert("employee_name".into(), "Employee Example".into());
        values.insert("manager_employee_id".into(), self.manager_employee_id.or(Some(self.id)).into());
        values.insert("manager_name".into(), Some("Manager Example".to_owned()).into());
        values.insert("appraisal_template_id".into(), self.template_id.into());
        values.insert("cycle_name".into(), "Annual".into());
        values.insert("cycle_start_date".into(), Utc::now().date_naive().into());
        values.insert("cycle_end_date".into(), Utc::now().date_naive().into());
        values.insert("cycle_stage".into(), self.stage_after_lock.into());
        values.insert("status".into(), "ACKNOWLEDGED".into());
        values.insert("self_submitted_at".into(), Option::<chrono::DateTime<Utc>>::None.into());
        values.insert("manager_submitted_at".into(), Option::<chrono::DateTime<Utc>>::None.into());
        values.insert("acknowledged_at".into(), acknowledgement.map(|(at, _)| at).into());
        values.insert("final_rating".into(), Option::<Decimal>::None.into());
        values.insert("performance_band".into(), Option::<String>::None.into());
        ProxyRow::new(values)
    }

    fn assert_locked_transaction(&self, table: &str, parent_id: Uuid, write: &str) {
        let events = self.events();
        let begin = events.iter().position(|e| e == "BEGIN").expect("write must start transaction");
        let lock = events.iter().position(|e| e.contains(table) && e.contains("FOR UPDATE")).expect("write must lock shared parent");
        let write = events.iter().position(|e| e.starts_with(write)).expect("expected business write");
        let commit = events.iter().position(|e| e == "COMMIT").expect("successful write must commit");
        assert!(begin < lock && lock < write && write < commit, "{events:#?}");
        assert!(events[lock].contains(&self.tenant.to_string()) && events[lock].contains(&parent_id.to_string()), "lock must scope tenant and actual parent id");
    }
}

#[async_trait::async_trait]
impl ProxyDatabaseTrait for Fixture {
    async fn begin(&self) { self.record("BEGIN"); }
    async fn commit(&self) { self.record("COMMIT"); }
    async fn rollback(&self) { self.record("ROLLBACK"); }
    fn start_rollback(&self) { self.record("ROLLBACK"); }

    async fn query(&self, statement: Statement) -> Result<Vec<ProxyRow>, DbErr> {
        let sql = statement.to_string();
        self.record(&sql);
        if sql.contains("FROM performance_participant p") && sql.contains("JOIN review_cycle c") {
            return Ok(vec![self.review_summary()]);
        }
        if sql.contains("COUNT") && sql.contains("FROM \"performance_participant\"") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "num_items".into(), i64::try_from(self.pending_acknowledgements).unwrap().into(),
            )]))]);
        }
        if sql.contains("FROM \"performance_participant\"") { return Ok(vec![self.participant()]); }
        if sql.contains("SELECT current_stage FROM review_cycle") {
            let stage = if sql.contains("FOR UPDATE") { self.stage_after_lock } else { "GOAL_SETTING" };
            return Ok(vec![ProxyRow::new(std::collections::BTreeMap::from([("current_stage".into(), stage.into())]))]);
        }
        if sql.contains("FROM performance_program p") {
            return Ok(vec![row(performance_program::Model {
                id: self.id, tenant_id: self.tenant, name: "Annual".into(), description: None,
                cadence: "MANUAL".into(), anchor_date: Utc::now().date_naive(), status: "ACTIVE".into(),
                include_calibration: false, include_acknowledgement: self.include_acknowledgement, goal_weight_required: Decimal::ONE_HUNDRED,
                rating_min: Decimal::ONE, rating_max: Decimal::new(5, 0), created_by: None, created_at: Utc::now(), updated_at: Utc::now(),
            })]);
        }
        if sql.contains("FROM \"appraisal_template_section\"") {
            return Ok(if self.empty_definition { vec![] } else { vec![row(appraisal_template_section::Model {
                id: self.id, tenant_id: self.tenant, appraisal_template_id: self.template_id, title: "General".into(), description: None, display_order: 0, created_at: Utc::now(),
            })] });
        }
        if sql.contains("FROM \"appraisal_question\"") {
            return Ok(vec![row(appraisal_question::Model { id: self.id, tenant_id: self.tenant, section_id: self.id,
                parent_question_id: None, question_type: "TEXT".into(), prompt: "Progress?".into(), is_required: true,
                answerer: "BOTH".into(), self_rating_enabled: false, manager_rating_enabled: false, display_order: 0, created_at: Utc::now() })]);
        }
        if sql.contains("FROM \"appraisal_question_option\"") { return Ok(vec![]); }
        if sql.contains("FROM \"appraisal_template\"") || sql.starts_with("UPDATE \"appraisal_template\"") {
            let published = self.events().iter().any(|e| e.starts_with("UPDATE \"appraisal_template\""));
            return Ok(vec![self.template(published)]);
        }
        if sql.starts_with("UPDATE \"performance_participant\"") { return Ok(vec![self.participant()]); }
        if sql.starts_with("INSERT INTO \"goal\"") || sql.starts_with("UPDATE \"goal\"") {
            if self.fail_goal_write { return Err(DbErr::Custom("goal write failure".into())); }
            return Ok(vec![self.goal(!sql.contains("PROPOSED"))]);
        }
        if sql.contains("COUNT") && sql.contains("FROM \"kpi\"") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "num_items".into(), if self.dependent == Some("KPI") { 1_i64 } else { 0_i64 }.into(),
            )]))]);
        }
        if sql.contains("COUNT") && sql.contains("FROM \"continuous_feedback\"") {
            return Ok(vec![ProxyRow::new(BTreeMap::from([(
                "num_items".into(), if self.dependent == Some("FEEDBACK") { 1_i64 } else { 0_i64 }.into(),
            )]))]);
        }
        if sql.contains("COUNT") && sql.contains("FROM \"goal\"") && sql.contains("parent_goal_id") {
            // A child goal is a dependency even when it belongs to another cycle. The source
            // guard must therefore not narrow this query by review_cycle_id.
            let count = if self.dependent == Some("CHILD") && !sql.contains("AND \"goal\".\"review_cycle_id\" =") { 1_i64 } else { 0_i64 };
            return Ok(vec![ProxyRow::new(BTreeMap::from([("num_items".into(), count.into())]))]);
        }
        if sql.contains("FROM \"goal\"") {
            let approved = self.initial_goals_approved || self.events().iter().any(|e| e.starts_with("UPDATE \"goal\""));
            return Ok(vec![self.goal(approved)]);
        }
        if sql.contains("FROM \"continuous_feedback\"") || sql.contains("FROM \"appraisal_answer\"") {
            return Ok(vec![]);
        }
        Err(DbErr::Custom(format!("unexpected query: {sql}")))
    }

    async fn execute(&self, statement: Statement) -> Result<ProxyExecResult, DbErr> {
        self.record(statement.to_string());
        Ok(ProxyExecResult { last_insert_id: 0, rows_affected: 1 })
    }
}

const PUBLISH: &str = "mutation { publishAppraisalTemplate(appraisalTemplateId: \"$TEMPLATE\") { status } }";
const PROPOSE: &str = "mutation { proposePerformanceGoal(input: { participantId: \"$PARTICIPANT\", title: \"Deliver\", weightage: \"100\" }) { status } }";
const APPROVE: &str = "mutation { approvePerformanceGoals(participantId: \"$PARTICIPANT\") { status } }";
const ADVANCE: &str = "mutation { advancePerformanceCycle(reviewCycleId: \"$CYCLE\") }";
const UPDATE: &str = "mutation { updatePerformanceGoal(goalId: \"$GOAL\", input: { participantId: \"$PARTICIPANT\", title: \"Refined\", description: \"Clarified scope\", weightage: \"100\" }) { id title status } }";
const DELETE: &str = "mutation { deletePerformanceGoal(participantId: \"$PARTICIPANT\", goalId: \"$GOAL\") }";
const ACKNOWLEDGE: &str = "mutation { acknowledgePerformanceReview(participantId: \"$PARTICIPANT\", comment: \"retry must not replace the first acknowledgement\") { review { acknowledgedAt } } }";

#[tokio::test]
async fn publish_locks_template_before_definition_read_and_status_write() {
    let fixture = Fixture::new();
    let result = fixture.run(PUBLISH).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["publishAppraisalTemplate"]["status"], "PUBLISHED");
    fixture.assert_locked_transaction("appraisal_template", fixture.template_id, "UPDATE \"appraisal_template\"");
    let events = fixture.events();
    let lock = events.iter().position(|e| e.contains("FOR UPDATE")).unwrap();
    let definition = events.iter().position(|e| e.contains("FROM \"appraisal_template_section\"")).unwrap();
    assert!(lock < definition);
}

#[tokio::test]
async fn publish_invalid_definition_rolls_back_without_status_write() {
    let mut fixture = Fixture::new(); fixture.empty_definition = true;
    let result = fixture.run(PUBLISH).await;
    assert!(result.errors.iter().any(|e| e.message.contains("requires questions")), "{:?}", result.errors);
    let events = fixture.events();
    assert!(events.iter().any(|e| e == "ROLLBACK"), "{events:#?}");
    assert!(!events.iter().any(|e| e == "COMMIT" || e.starts_with("UPDATE")));
}

#[tokio::test]
async fn proposal_holds_cycle_lock_until_goal_insert_commits() {
    let fixture = Fixture::new();
    let result = fixture.run(PROPOSE).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["proposePerformanceGoal"]["status"], "PROPOSED");
    fixture.assert_locked_transaction("review_cycle", fixture.cycle_id, "INSERT INTO \"goal\"");
}

#[tokio::test]
async fn approval_locks_cycle_before_loading_goal_weights_and_updating() {
    let fixture = Fixture::new();
    let result = fixture.run(APPROVE).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["approvePerformanceGoals"][0]["status"], "APPROVED");
    fixture.assert_locked_transaction("review_cycle", fixture.cycle_id, "UPDATE \"goal\"");
    let events = fixture.events();
    let lock = events.iter().position(|e| e.contains("FOR UPDATE")).unwrap();
    let goals = events.iter().position(|e| e.contains("FROM \"goal\"")).unwrap();
    assert!(lock < goals);
}

#[tokio::test]
async fn goal_mutations_recheck_stage_after_lock_and_do_not_write_after_advance() {
    for operation in [PROPOSE, APPROVE] {
        let mut fixture = Fixture::new(); fixture.stage_after_lock = "SELF_REVIEW";
        let result = fixture.run(operation).await;
        assert!(result.errors.iter().any(|e| e.message.contains("during goal setting")), "{:?}", result.errors);
        let events = fixture.events();
        assert!(events.iter().any(|e| e == "ROLLBACK"), "{events:#?}");
        assert!(!events.iter().any(|e| e == "COMMIT" || e.contains("FROM \"goal\"") || e.starts_with("INSERT") || e.starts_with("UPDATE")), "{events:#?}");
    }
}

#[tokio::test]
async fn goal_write_failure_rolls_back_without_commit() {
    for operation in [PROPOSE, APPROVE] {
        let mut fixture = Fixture::new(); fixture.fail_goal_write = true;
        let result = fixture.run(operation).await;
        assert!(!result.errors.is_empty());
        let events = fixture.events();
        assert!(events.iter().any(|e| e == "ROLLBACK"), "{events:#?}");
        assert!(!events.iter().any(|e| e == "COMMIT"));
    }
}

#[tokio::test]
async fn advance_uses_same_cycle_lock_through_stage_write() {
    let mut fixture = Fixture::new(); fixture.initial_goals_approved = true;
    let result = fixture.run(ADVANCE).await;
    assert!(result.errors.is_empty(), "{:?}", result.errors);
    assert_eq!(result.data.into_json().unwrap()["advancePerformanceCycle"], "SELF_REVIEW");
    fixture.assert_locked_transaction("review_cycle", fixture.cycle_id, "UPDATE review_cycle");
}

#[tokio::test]
async fn goal_authority_matrix_is_enforced_by_graphql_resolvers() {
    let admin_without_employee = Fixture::new().with_claims("performance:manage", "ALL", None);
    assert!(admin_without_employee.run(PROPOSE).await.errors.is_empty());

    let own_employee = Fixture::new()
        .with_claims("performance:self", "SELF", None)
        .own_employee();
    assert!(own_employee.run(PROPOSE).await.errors.is_empty());

    let assigned_manager = Fixture::new().assigned_manager();
    assert!(assigned_manager.run(PROPOSE).await.errors.is_empty());

    let unrelated_manager = Fixture::new()
        .with_claims("performance:evaluate", "TEAM", Some(Uuid::new_v4()));
    let response = unrelated_manager.run(PROPOSE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("not authorized")), "{:?}", response.errors);

    let team_without_employee = Fixture::new().with_claims("performance:evaluate", "TEAM", None);
    let response = team_without_employee.run(PROPOSE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("employee-linked")), "{:?}", response.errors);
    assert!(!team_without_employee.events().iter().any(|event| event == "BEGIN"));
}

#[tokio::test]
async fn employee_goal_correction_is_limited_to_proposed_goals() {
    let employee = Fixture::new()
        .with_claims("performance:self", "SELF", None)
        .own_employee();
    let mut approved = employee.clone();
    approved.initial_goals_approved = true;
    let response = approved.run(UPDATE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("only change proposed")), "{:?}", response.errors);
    assert!(!approved.events().iter().any(|event| event.starts_with("UPDATE \"goal\"")));

    let manager = Fixture::new().assigned_manager();
    let mut approved_for_manager = manager.clone();
    approved_for_manager.initial_goals_approved = true;
    let response = approved_for_manager.run(UPDATE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(response.data.into_json().unwrap()["updatePerformanceGoal"]["status"], "PROPOSED");
}

#[tokio::test]
async fn goal_resolvers_reject_excluded_and_advanced_reviews_without_writes() {
    for operation in [PROPOSE, UPDATE, DELETE, APPROVE] {
        let mut excluded = Fixture::new();
        excluded.is_excluded = true;
        let response = excluded.run(operation).await;
        assert!(response.errors.iter().any(|error| error.message.contains("Excluded participants")), "{operation}: {:?}", response.errors);
        assert!(!excluded.events().iter().any(|event| event.starts_with("INSERT INTO \"goal\"") || event.starts_with("UPDATE \"goal\"") || event.starts_with("DELETE FROM \"goal\"")));

        let mut advanced = Fixture::new();
        advanced.stage_after_lock = "SELF_REVIEW";
        let response = advanced.run(operation).await;
        assert!(response.errors.iter().any(|error| error.message.contains("during goal setting")), "{operation}: {:?}", response.errors);
        assert!(!advanced.events().iter().any(|event| event.starts_with("INSERT INTO \"goal\"") || event.starts_with("UPDATE \"goal\"") || event.starts_with("DELETE FROM \"goal\"")));
    }
}

#[tokio::test]
async fn goal_approval_rejects_missing_weights_before_status_write() {
    let mut fixture = Fixture::new();
    fixture.missing_goal_weight = true;
    let response = fixture.run(APPROVE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("Every goal must have a weight")), "{:?}", response.errors);
    assert!(!fixture.events().iter().any(|event| event.starts_with("UPDATE \"goal\"")));
}

#[tokio::test]
async fn update_and_delete_reset_goal_approval_state_with_participant_scope() {
    let mut update = Fixture::new();
    update.initial_goals_approved = true;
    let response = update.run(UPDATE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let update_events = update.events();
    let update_sql = update_events.iter().find(|event| event.starts_with("UPDATE \"goal\"")).expect("edit must update the goal");
    assert!(update_sql.contains("PROPOSED"));
    let lookup_sql = update_events.iter().find(|event| event.contains("FROM \"goal\"")).expect("edit must scope the target lookup");
    assert!(lookup_sql.contains(&update.tenant.to_string()) && lookup_sql.contains(&update.cycle_id.to_string()) && lookup_sql.contains(&update.employee_id.to_string()));

    let mut delete = Fixture::new();
    delete.initial_goals_approved = true;
    let response = delete.run(DELETE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(response.data.into_json().unwrap()["deletePerformanceGoal"], true);
    let reset_sql = delete.events().into_iter().find(|event| event.starts_with("UPDATE \"goal\"")).expect("delete must reset remaining goals");
    assert!(reset_sql.contains("PROPOSED"));
    assert!(reset_sql.contains(&delete.tenant.to_string()) && reset_sql.contains(&delete.cycle_id.to_string()) && reset_sql.contains(&delete.employee_id.to_string()));
}

#[tokio::test]
async fn deletion_preserves_kpi_feedback_and_cross_cycle_child_evidence() {
    for dependency in ["KPI", "FEEDBACK", "CHILD"] {
        let mut fixture = Fixture::new();
        fixture.dependent = Some(dependency);
        let response = fixture.run(DELETE).await;
        assert!(response.errors.iter().any(|error| error.message.contains("dependent KPI, child goal, or feedback")), "{dependency}: {:?} {:#?}", response.errors, fixture.events());
        assert!(!fixture.events().iter().any(|event| event.starts_with("DELETE FROM \"goal\"")), "{dependency}: {:#?}", fixture.events());
    }
}

#[tokio::test]
async fn acknowledgement_is_first_write_wins_and_rejects_invalid_participants_or_stages() {
    let mut first = Fixture::new().with_claims("performance:self", "SELF", None).own_employee();
    first.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    let response = first.run(ACKNOWLEDGE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    let write = first.events().into_iter().find(|event| event.starts_with("UPDATE \"performance_participant\""))
        .expect("first acknowledgement must issue a participant update");
    assert!(write.contains("acknowledged_at") && write.contains("acknowledgement_comment")
        && write.contains("updated_at") && write.contains("ACKNOWLEDGED"), "{write}");
    assert!(write.contains("retry must not replace the first acknowledgement"), "{write}");

    let mut retry = Fixture::new().with_claims("performance:self", "SELF", None).own_employee().acknowledged("first acknowledgement");
    retry.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    let stored = retry.acknowledgement().expect("fixture acknowledgement");
    let response = retry.run(ACKNOWLEDGE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(retry.acknowledgement().expect("retry keeps acknowledgement"), stored);
    assert!(!retry.events().iter().any(|event| event.starts_with("UPDATE \"performance_participant\"")));

    let mut stale = Fixture::new().with_claims("performance:self", "SELF", None).own_employee();
    stale.stage_after_lock = "CLOSED";
    let response = stale.run(ACKNOWLEDGE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("not awaiting employee acknowledgement")), "{:?}", response.errors);

    let mut excluded = Fixture::new().with_claims("performance:self", "SELF", None).own_employee();
    excluded.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    excluded.is_excluded = true;
    let response = excluded.run(ACKNOWLEDGE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("Excluded participants")), "{:?}", response.errors);

    let mut manager = Fixture::new().assigned_manager();
    manager.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    let response = manager.run(ACKNOWLEDGE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("performance:self")), "{:?}", response.errors);
    assert!(!manager.events().iter().any(|event| event == "BEGIN"));

    let mut another_employee = Fixture::new().with_claims("performance:self", "SELF", Some(Uuid::new_v4()));
    another_employee.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    let response = another_employee.run(ACKNOWLEDGE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("another employee")), "{:?}", response.errors);
    assert!(!another_employee.events().iter().any(|event| event.starts_with("UPDATE \"performance_participant\"")));
}

#[tokio::test]
async fn employee_goal_deletion_is_limited_to_proposed_goals() {
    let employee = Fixture::new().with_claims("performance:self", "SELF", None).own_employee();
    let response = employee.run(DELETE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(response.data.into_json().unwrap()["deletePerformanceGoal"], true);

    let mut approved = Fixture::new().with_claims("performance:self", "SELF", None).own_employee();
    approved.initial_goals_approved = true;
    let response = approved.run(DELETE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("only change proposed")), "{:?}", response.errors);
    assert!(!approved.events().iter().any(|event| event.starts_with("DELETE FROM \"goal\"")));
}

#[tokio::test]
async fn acknowledgement_stage_cannot_close_until_every_active_employee_acknowledges() {
    let mut pending = Fixture::new();
    pending.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    pending.pending_acknowledgements = 1;
    let response = pending.run(ADVANCE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("acknowledgement(s) are still pending")), "{:?}", response.errors);
    assert!(!pending.events().iter().any(|event| event.starts_with("UPDATE review_cycle")));

    let mut complete = Fixture::new();
    complete.stage_after_lock = "EMPLOYEE_ACKNOWLEDGEMENT";
    let response = complete.run(ADVANCE).await;
    assert!(response.errors.is_empty(), "{:?}", response.errors);
    assert_eq!(response.data.into_json().unwrap()["advancePerformanceCycle"], "CLOSED");
    complete.assert_locked_transaction("review_cycle", complete.cycle_id, "UPDATE review_cycle");
}

#[tokio::test]
async fn goal_mutation_fails_closed_when_the_participant_cycle_changes_during_locking() {
    let mut fixture = Fixture::new();
    fixture.participant_cycle_after_lock = Some(Uuid::new_v4());
    let response = fixture.run(PROPOSE).await;
    assert!(response.errors.iter().any(|error| error.message.contains("changed while acquiring its cycle lock")), "{:?}", response.errors);
    assert!(!fixture.events().iter().any(|event| event.starts_with("INSERT INTO \"goal\"")));
}

const DISPOSABLE_POSTGRES_URL: &str = "postgresql://hrms_perf_test@127.0.0.1:55449/postgres";
const POSTGRES_LOCK_PROOF_TIMEOUT: Duration = Duration::from_secs(5);

fn disposable_postgres_url() -> Option<String> {
    match env::var("HRMS_PERFORMANCE_TEST_DATABASE_URL") {
        Ok(url) if url == DISPOSABLE_POSTGRES_URL => Some(url),
        Ok(_) => panic!("HRMS_PERFORMANCE_TEST_DATABASE_URL must name only the approved local disposable cluster"),
        Err(env::VarError::NotPresent) => None,
        Err(error) => panic!("HRMS_PERFORMANCE_TEST_DATABASE_URL is invalid: {error}"),
    }
}

async fn wait_for_feedback_insert_to_block(db: &DatabaseConnection, feedback_pid: i32, deleting_pid: i32) -> Result<(), String> {
    let deadline = Instant::now() + POSTGRES_LOCK_PROOF_TIMEOUT;
    loop {
        let statement = Statement::from_string(DbBackend::Postgres, format!(
            "SELECT COALESCE((SELECT state = 'active' AND wait_event_type = 'Lock' AND {deleting_pid} = ANY(pg_blocking_pids(pid)) FROM pg_stat_activity WHERE pid = {feedback_pid}), FALSE) AS is_blocked"
        ));
        let row = db.query_one(statement).await.map_err(|error| format!("inspect feedback backend state: {error}"))?
            .ok_or_else(|| "PostgreSQL did not return feedback backend state".to_owned())?;
        let is_blocked: bool = row.try_get("", "is_blocked")
            .map_err(|error| format!("read feedback backend lock state: {error}"))?;
        if is_blocked {
            return Ok(());
        }
        if Instant::now() >= deadline {
            return Err(format!("feedback INSERT did not reach an active PostgreSQL lock wait specifically blocked by deleting backend {deleting_pid} within {:?}", POSTGRES_LOCK_PROOF_TIMEOUT));
        }
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

fn sqlstate(error: &DbErr) -> Option<String> {
    match error {
        DbErr::Exec(RuntimeErr::SqlxError(SqlxError::Database(database_error)))
        | DbErr::Query(RuntimeErr::SqlxError(SqlxError::Database(database_error))) => {
            database_error.code().map(|code| code.into_owned())
        }
        _ => None,
    }
}

async fn postgres_feedback_lock_proof(db: &DatabaseConnection, url: &str, quoted_schema: &str, goal_id: Uuid) -> Result<(), String> {
    let deleting = db.begin().await.map_err(|error| format!("begin deleting transaction: {error}"))?;
    let deleting_pid_row = deleting.query_one(Statement::from_string(DbBackend::Postgres, "SELECT pg_backend_pid() AS backend_pid".to_owned()))
        .await.map_err(|error| format!("read deleting backend PID: {error}"))?
        .ok_or_else(|| "PostgreSQL did not return a deleting backend PID".to_owned())?;
    let deleting_pid: i32 = deleting_pid_row.try_get("", "backend_pid")
        .map_err(|error| format!("decode deleting backend PID: {error}"))?;
    deleting.query_one(Statement::from_string(DbBackend::Postgres, format!("SELECT id FROM {quoted_schema}.goal WHERE id = '{goal_id}' FOR UPDATE")))
        .await.map_err(|error| format!("lock target goal: {error}"))?;

    let mut feedback_options = ConnectOptions::new(url);
    feedback_options.max_connections(1).connect_timeout(POSTGRES_LOCK_PROOF_TIMEOUT);
    let feedback_db = Database::connect(feedback_options).await.map_err(|error| format!("open independent feedback session: {error}"))?;
    let feedback_txn = feedback_db.begin().await.map_err(|error| format!("begin feedback transaction: {error}"))?;
    let feedback_pid_row = feedback_txn.query_one(Statement::from_string(DbBackend::Postgres, "SELECT pg_backend_pid() AS backend_pid".to_owned()))
        .await.map_err(|error| format!("read feedback backend PID: {error}"))?
        .ok_or_else(|| "PostgreSQL did not return a feedback backend PID".to_owned())?;
    let feedback_pid: i32 = feedback_pid_row.try_get("", "backend_pid")
        .map_err(|error| format!("decode feedback backend PID: {error}"))?;
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let feedback_schema = quoted_schema.to_owned();
    let feedback_id = Uuid::new_v4();
    let mut feedback = Some(tokio::spawn(async move {
        let _ = started_tx.send(());
        let execution = feedback_txn.execute(Statement::from_string(DbBackend::Postgres, format!(
            "INSERT INTO {feedback_schema}.continuous_feedback (id, goal_id) VALUES ('{feedback_id}', '{goal_id}')"
        ))).await;
        let rollback = feedback_txn.rollback().await;
        match (execution, rollback) {
            (Ok(result), Ok(())) => Ok(result),
            (Err(error), Ok(())) => Err(error),
            (_, Err(error)) => Err(error),
        }
    }));

    let mut deleting = Some(deleting);
    let outcome = async {
        tokio::time::timeout(POSTGRES_LOCK_PROOF_TIMEOUT, started_rx).await
            .map_err(|_| format!("feedback INSERT task did not start within {:?}", POSTGRES_LOCK_PROOF_TIMEOUT))?
            .map_err(|_| "feedback INSERT task exited before executing".to_owned())?;
        wait_for_feedback_insert_to_block(db, feedback_pid, deleting_pid).await?;
        deleting.as_ref().ok_or_else(|| "deleting transaction was unexpectedly consumed before commit".to_owned())?
            .execute(Statement::from_string(DbBackend::Postgres, format!("DELETE FROM {quoted_schema}.goal WHERE id = '{goal_id}'")))
            .await.map_err(|error| format!("delete locked goal: {error}"))?;
        deleting.take().ok_or_else(|| "deleting transaction was unexpectedly unavailable at commit".to_owned())?.commit().await
            .map_err(|error| format!("commit goal deletion: {error}"))?;
        let joined_feedback = match tokio::time::timeout(
            POSTGRES_LOCK_PROOF_TIMEOUT,
            feedback.as_mut().ok_or_else(|| "feedback task was unexpectedly unavailable".to_owned())?,
        ).await {
            Ok(joined) => {
                feedback.take();
                joined
            }
            Err(_) => return Err(format!("feedback INSERT did not finish within {:?}", POSTGRES_LOCK_PROOF_TIMEOUT)),
        };
        let feedback_result = joined_feedback.map_err(|error| format!("feedback task panicked: {error}"))?;
        let error = match feedback_result {
            Ok(_) => return Err("blocked feedback INSERT unexpectedly succeeded after its referenced goal was deleted".to_owned()),
            Err(error) => error,
        };
        if sqlstate(&error).as_deref() != Some("23503") {
            return Err(format!("feedback INSERT must fail with PostgreSQL foreign-key SQLSTATE 23503, got {error}"));
        }
        Ok(())
    }.await;

    if let Some(mut task) = feedback {
        task.abort();
        let _ = tokio::time::timeout(POSTGRES_LOCK_PROOF_TIMEOUT, &mut task).await;
    }
    if let Some(transaction) = deleting {
        transaction.rollback().await.map_err(|error| format!("rollback deleting transaction after failed proof: {error}"))?;
    }
    outcome
}

#[tokio::test]
async fn postgresql_feedback_insert_cannot_outlive_a_locked_goal_deletion() {
    let Some(url) = disposable_postgres_url() else {
        eprintln!("skipping PostgreSQL lock proof: HRMS_PERFORMANCE_TEST_DATABASE_URL is not set");
        return;
    };
    let schema = format!("performance_lock_test_{}", Uuid::new_v4().simple());
    let quoted_schema = format!("\"{schema}\"");
    let db = Database::connect(&url).await.expect("connect to approved disposable PostgreSQL cluster");
    let proof = async {
        db.execute(Statement::from_string(DbBackend::Postgres, format!("CREATE SCHEMA {quoted_schema}"))).await?;
        db.execute(Statement::from_string(DbBackend::Postgres, format!("CREATE TABLE {quoted_schema}.goal (id UUID PRIMARY KEY)"))).await?;
        db.execute(Statement::from_string(DbBackend::Postgres, format!("CREATE TABLE {quoted_schema}.continuous_feedback (id UUID PRIMARY KEY, goal_id UUID REFERENCES {quoted_schema}.goal(id) ON DELETE SET NULL)"))).await?;
        let goal_id = Uuid::new_v4();
        db.execute(Statement::from_string(DbBackend::Postgres, format!("INSERT INTO {quoted_schema}.goal (id) VALUES ('{goal_id}')"))).await?;
        postgres_feedback_lock_proof(&db, &url, &quoted_schema, goal_id).await.map_err(DbErr::Custom)
    }.await;
    let cleanup = tokio::time::timeout(POSTGRES_LOCK_PROOF_TIMEOUT, db.execute(Statement::from_string(DbBackend::Postgres, format!("DROP SCHEMA IF EXISTS {quoted_schema} CASCADE")))).await;
    match (proof, cleanup) {
        (Ok(()), Ok(Ok(_))) => {}
        (Err(proof_error), Ok(Ok(_))) => panic!("PostgreSQL lock proof failed: {proof_error}"),
        (Ok(()), Ok(Err(cleanup_error))) => panic!("PostgreSQL lock proof passed but schema cleanup failed: {cleanup_error}"),
        (Err(proof_error), Ok(Err(cleanup_error))) => panic!("PostgreSQL lock proof failed: {proof_error}; schema cleanup also failed: {cleanup_error}"),
        (Ok(()), Err(_)) => panic!("PostgreSQL lock proof passed but schema cleanup exceeded {:?}", POSTGRES_LOCK_PROOF_TIMEOUT),
        (Err(proof_error), Err(_)) => panic!("PostgreSQL lock proof failed: {proof_error}; schema cleanup exceeded {:?}", POSTGRES_LOCK_PROOF_TIMEOUT),
    }
}
