//! GraphQL DTOs for kabipay-leave.

use async_graphql::{ComplexObject, Context, InputObject, Result, SimpleObject, ID};
use chrono::{DateTime, NaiveDate, Utc};
use kabipay_common::client_data_scope::resolve_viewer_employee;
use kabipay_common::context::{ClientClaims, ScopeType, PERM_LEAVE_APPROVE};
use kabipay_common::subgraph::{require_client_claims, require_tenant_id, tenant_db};
use kabipay_common::workflow_approval::WorkflowApprovalAuthority;
use kabipay_common::KabiPayError;
use kabipay_db_entities::tenant::d0011_leave::{leave_balance, leave_policy, leave_request, leave_type};
use kabipay_db_entities::tenant::d0025_workflow::workflow_action;

use crate::resolvers::query::parse_uuid;
use crate::services::leave_service;
use std::future::Future;
use std::sync::Arc;
use tokio::sync::OnceCell;

#[derive(Clone, Debug, Default)]
struct LeaveApprovalSnapshotCache {
    value: Arc<OnceCell<Option<uuid::Uuid>>>,
}

impl LeaveApprovalSnapshotCache {
    async fn get_or_try_init<E, F, Fut>(&self, init: F) -> std::result::Result<Option<uuid::Uuid>, E>
    where
        F: FnOnce() -> Fut,
        Fut: Future<Output = std::result::Result<Option<uuid::Uuid>, E>>,
    {
        self.value.get_or_try_init(init).await.copied()
    }
}

fn leave_approval_scope_from_claims(claims: &ClientClaims) -> Option<ScopeType> {
    if !claims.has_any_permission(&[PERM_LEAVE_APPROVE]) {
        return None;
    }
    claims
        .scope_for_permission(PERM_LEAVE_APPROVE)
        .filter(|scope| matches!(scope, ScopeType::Team | ScopeType::All))
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveType")]
pub struct LeaveTypeDto {
    pub id: ID,
    pub tenant_id: ID,
    pub name: String,
    pub code: String,
    pub is_paid: bool,
    pub carry_forward: bool,
    pub max_carry_forward_days: Option<i32>,
    pub sandwich_rule: bool,
    pub half_day_allowed: bool,
    pub requires_document: bool,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<leave_type::Model> for LeaveTypeDto {
    fn from(m: leave_type::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            name: m.name,
            code: m.code,
            is_paid: m.is_paid,
            carry_forward: m.carry_forward,
            max_carry_forward_days: m.max_carry_forward_days,
            sandwich_rule: m.sandwich_rule,
            half_day_allowed: m.half_day_allowed,
            requires_document: m.requires_document,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeavePolicy")]
pub struct LeavePolicyDto {
    pub id: ID,
    pub tenant_id: ID,
    pub leave_type_id: ID,
    pub applicable_to: Option<String>,
    pub annual_entitlement: Option<i32>,
    pub accrual_frequency: Option<String>,
    pub accrual_days: Option<String>,
    pub max_consecutive_days: Option<i32>,
    pub min_notice_days: Option<i32>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<leave_policy::Model> for LeavePolicyDto {
    fn from(m: leave_policy::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            leave_type_id: ID(m.leave_type_id.to_string()),
            applicable_to: m.applicable_to,
            annual_entitlement: m.annual_entitlement,
            accrual_frequency: m.accrual_frequency,
            accrual_days: m.accrual_days.map(|d| d.to_string()),
            max_consecutive_days: m.max_consecutive_days,
            min_notice_days: m.min_notice_days,
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveWorkflowAction")]
pub struct LeaveWorkflowActionDto {
    pub workflow_step_name: String,
    pub action: String,
    pub remarks: Option<String>,
    pub acted_at: DateTime<Utc>,
    pub performed_by_user_id: Option<ID>,
}

impl LeaveWorkflowActionDto {
    pub fn from_action(step_name: String, a: workflow_action::Model) -> Self {
        Self {
            workflow_step_name: step_name,
            action: a.action,
            remarks: a.remarks,
            acted_at: a.acted_at,
            performed_by_user_id: a.performed_by.map(|u| ID(u.to_string())),
        }
    }
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(complex)]
#[graphql(name = "LeaveRequest")]
pub struct LeaveRequestDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub employee_name: Option<String>,
    pub employee_code: Option<String>,
    pub leave_type_id: ID,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    /// Days requested, serialised as a decimal string for lossless transport.
    pub days_requested: String,
    pub is_half_day: bool,
    pub half_day_session: Option<String>,
    pub status: String,
    pub reason: Option<String>,
    pub rejection_reason: Option<String>,
    /// Link or reference ID when the leave type requires documentation.
    pub supporting_document_reference: Option<String>,
    pub applied_at: DateTime<Utc>,
    /// Set when tenant has an active **LEAVE_REQUEST** workflow with at least one step (M8).
    pub workflow_instance_id: Option<ID>,
    #[graphql(skip)]
    approval_snapshot_cache: LeaveApprovalSnapshotCache,
}

#[derive(SimpleObject, Clone, Debug)]
#[graphql(name = "LeaveBalance")]
pub struct LeaveBalanceDto {
    pub id: ID,
    pub tenant_id: ID,
    pub employee_id: ID,
    pub leave_type_id: ID,
    pub year: i32,
    pub entitled_days: String,
    pub used_days: String,
    pub pending_days: String,
    pub carried_forward_days: String,
    pub balance_days: String,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}

impl From<leave_balance::Model> for LeaveBalanceDto {
    fn from(m: leave_balance::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            leave_type_id: ID(m.leave_type_id.to_string()),
            year: m.year,
            entitled_days: m.entitled_days.to_string(),
            used_days: m.used_days.to_string(),
            pending_days: m.pending_days.to_string(),
            carried_forward_days: m.carried_forward_days.to_string(),
            balance_days: m.balance_days.to_string(),
            created_at: m.created_at,
            updated_at: m.updated_at,
        }
    }
}

#[ComplexObject]
impl LeaveRequestDto {
    async fn actionable_approval_step_id(&self, ctx: &Context<'_>) -> Result<Option<uuid::Uuid>> {
        let claims = require_client_claims(ctx)?;
        let Some(scope) = leave_approval_scope_from_claims(claims) else {
            return Ok(None);
        };
        self.approval_snapshot_cache
            .get_or_try_init(|| async {
                let tenant_id = require_tenant_id(ctx)?;
                let db = tenant_db(ctx, tenant_id).await?;
                let authority = WorkflowApprovalAuthority {
                    actor_user_id: claims.sub,
                    actor_employee: resolve_viewer_employee(ctx, &db, tenant_id).await?,
                    scope,
                    permission: PERM_LEAVE_APPROVE,
                };
                let request_id = parse_uuid(&self.id, "leaveRequestId")?;
                let subject_employee_id = parse_uuid(&self.employee_id, "employeeId")?;
                let workflow_instance_id = self
                    .workflow_instance_id
                    .as_ref()
                    .map(|id| parse_uuid(id, "workflowInstanceId"))
                    .transpose()?;
                leave_service::resolve_actionable_leave_workflow_step_id(
                    &db,
                    tenant_id,
                    request_id,
                    &self.status,
                    subject_employee_id,
                    workflow_instance_id,
                    &authority,
                )
                .await
                .map_err(KabiPayError::into_graphql)
            })
            .await
    }

    async fn pending_approval_stage(&self, ctx: &Context<'_>) -> Result<Option<String>> {
        let tenant_id = require_tenant_id(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let workflow_instance_id = self
            .workflow_instance_id
            .as_ref()
            .map(|id| parse_uuid(id, "workflowInstanceId"))
            .transpose()?;
        leave_service::resolve_leave_pending_approval_stage(
            &db,
            tenant_id,
            &self.status,
            workflow_instance_id,
        )
        .await
        .map_err(KabiPayError::into_graphql)
    }

    async fn viewer_may_approve(&self, ctx: &Context<'_>) -> Result<bool> {
        Ok(self.actionable_approval_step_id(ctx).await?.is_some())
    }

    async fn pending_approval_step_id(&self, ctx: &Context<'_>) -> Result<Option<ID>> {
        self.actionable_approval_step_id(ctx)
            .await
            .map(|step_id| step_id.map(|id| ID(id.to_string())))
    }
}

impl LeaveRequestDto {
    pub fn with_employee_label(mut self, name: String, code: String) -> Self {
        self.employee_name = Some(name);
        self.employee_code = Some(code);
        self
    }
}

#[derive(InputObject, Clone, Debug)]
pub struct SubmitLeaveRequestInput {
    pub leave_type_id: ID,
    pub from_date: NaiveDate,
    pub to_date: NaiveDate,
    pub is_half_day: bool,
    pub half_day_session: Option<String>,
    pub reason: Option<String>,
    pub supporting_document_reference: Option<String>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertLeaveTypeInput {
    pub id: Option<ID>,
    pub name: String,
    pub code: String,
    pub is_paid: bool,
    pub carry_forward: bool,
    pub max_carry_forward_days: Option<i32>,
    pub sandwich_rule: bool,
    pub half_day_allowed: bool,
    pub requires_document: bool,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertLeavePolicyInput {
    pub id: Option<ID>,
    pub leave_type_id: ID,
    pub applicable_to: Option<String>,
    pub annual_entitlement: Option<i32>,
    pub accrual_frequency: Option<String>,
    pub accrual_days: Option<String>,
    pub max_consecutive_days: Option<i32>,
    pub min_notice_days: Option<i32>,
}

#[derive(InputObject, Clone, Debug)]
pub struct UpsertLeaveBalanceInput {
    pub employee_id: ID,
    pub leave_type_id: ID,
    pub year: i32,
    pub entitled_days: String,
    pub used_days: String,
    pub pending_days: String,
    pub carried_forward_days: String,
}

#[derive(InputObject, Clone, Debug)]
pub struct AdjustLeaveBalanceEntitlementInput {
    pub employee_id: ID,
    pub leave_type_id: ID,
    pub year: i32,
    pub entitled_delta: String,
}

impl From<leave_request::Model> for LeaveRequestDto {
    fn from(m: leave_request::Model) -> Self {
        Self {
            id: ID(m.id.to_string()),
            tenant_id: ID(m.tenant_id.to_string()),
            employee_id: ID(m.employee_id.to_string()),
            employee_name: None,
            employee_code: None,
            leave_type_id: ID(m.leave_type_id.to_string()),
            from_date: m.from_date,
            to_date: m.to_date,
            days_requested: m.days_requested.to_string(),
            is_half_day: m.is_half_day,
            half_day_session: m.half_day_session,
            status: m.status,
            reason: m.reason,
            rejection_reason: m.rejection_reason,
            supporting_document_reference: m.supporting_document_reference,
            applied_at: m.applied_at,
            workflow_instance_id: m.workflow_instance_id.map(|u| ID(u.to_string())),
            approval_snapshot_cache: LeaveApprovalSnapshotCache::default(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Request, Schema};
    use chrono::NaiveDate;
    use kabipay_common::context::CLIENT_JWT_ISSUER;
    use std::collections::HashMap;
    use std::sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    };
    use uuid::Uuid;

    fn approval_claims(scope: Option<&str>) -> ClientClaims {
        let mut permission_scopes = HashMap::new();
        if let Some(scope) = scope {
            permission_scopes.insert(PERM_LEAVE_APPROVE.into(), scope.into());
        }
        ClientClaims {
            sub: Uuid::new_v4(),
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            tenant_id: Uuid::new_v4(),
            email: String::new(),
            employee_id: Some(Uuid::new_v4()),
            must_change_password: false,
            roles: vec![],
            permissions: vec![PERM_LEAVE_APPROVE.into()],
            permission_scopes,
            resource_scopes: HashMap::from([("leave".into(), "ALL".into())]),
        }
    }

    #[test]
    fn leave_approval_scope_requires_a_valid_exact_permission_scope() {
        assert_eq!(leave_approval_scope_from_claims(&approval_claims(None)), None);
        assert_eq!(
            leave_approval_scope_from_claims(&approval_claims(Some("INVALID"))),
            None
        );
        assert_eq!(
            leave_approval_scope_from_claims(&approval_claims(Some("TEAM"))),
            Some(ScopeType::Team)
        );
        assert_eq!(
            leave_approval_scope_from_claims(&approval_claims(Some("ALL"))),
            Some(ScopeType::All)
        );
        assert_eq!(
            leave_approval_scope_from_claims(&approval_claims(Some("SELF"))),
            None
        );
        assert_eq!(
            leave_approval_scope_from_claims(&approval_claims(Some("DEPARTMENT"))),
            None
        );
    }

    #[tokio::test]
    async fn approval_snapshot_cache_shares_one_concurrent_authorization_lookup() {
        let cache = LeaveApprovalSnapshotCache::default();
        let calls = Arc::new(AtomicUsize::new(0));
        let step_id = Uuid::new_v4();

        let first_calls = Arc::clone(&calls);
        let second_calls = Arc::clone(&calls);
        let (first, second) = tokio::join!(
            cache.get_or_try_init(|| async move {
                first_calls.fetch_add(1, Ordering::SeqCst);
                tokio::task::yield_now().await;
                Ok::<_, KabiPayError>(Some(step_id))
            }),
            cache.get_or_try_init(|| async move {
                second_calls.fetch_add(1, Ordering::SeqCst);
                Ok::<_, KabiPayError>(Some(Uuid::new_v4()))
            })
        );

        assert_eq!(first.expect("first approval snapshot"), Some(step_id));
        assert_eq!(second.expect("second approval snapshot"), Some(step_id));
        assert_eq!(calls.load(Ordering::SeqCst), 1);
    }

    struct TestQuery;

    #[Object]
    impl TestQuery {
        async fn leave_request(&self) -> LeaveRequestDto {
            LeaveRequestDto {
                id: ID(Uuid::new_v4().to_string()),
                tenant_id: ID(Uuid::new_v4().to_string()),
                employee_id: ID(Uuid::new_v4().to_string()),
                employee_name: None,
                employee_code: None,
                leave_type_id: ID(Uuid::new_v4().to_string()),
                from_date: NaiveDate::from_ymd_opt(2026, 8, 27).expect("valid date"),
                to_date: NaiveDate::from_ymd_opt(2026, 8, 27).expect("valid date"),
                days_requested: "1".into(),
                is_half_day: false,
                half_day_session: None,
                status: "PENDING".into(),
                reason: None,
                rejection_reason: None,
                supporting_document_reference: None,
                applied_at: Utc::now(),
                workflow_instance_id: Some(ID(Uuid::new_v4().to_string())),
                approval_snapshot_cache: LeaveApprovalSnapshotCache::default(),
            }
        }
    }

    #[test]
    fn leave_request_schema_exposes_server_authoritative_pending_approval_step_id() {
        let schema = Schema::build(TestQuery, EmptyMutation, EmptySubscription).finish();
        let sdl = schema.sdl();
        assert!(
            sdl.contains("pendingApprovalStepId: ID"),
            "LeaveRequest must expose the actionable step id: {sdl}"
        );
    }

    #[tokio::test]
    async fn pending_approval_step_id_is_none_without_exact_approval_permission() {
        let mut claims = approval_claims(Some("TEAM"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        let response = Schema::build(TestQuery, EmptyMutation, EmptySubscription)
            .data(claims)
            .finish()
            .execute(Request::new(
                "{ leaveRequest { pendingApprovalStepId } }",
            ))
            .await;
        assert!(response.errors.is_empty(), "unexpected response: {response:?}");
        assert_eq!(
            response.data.into_json().expect("GraphQL JSON"),
            serde_json::json!({"leaveRequest": {"pendingApprovalStepId": null}})
        );
    }

    #[tokio::test]
    async fn approval_snapshot_fields_both_fail_closed_without_exact_permission() {
        let mut claims = approval_claims(Some("TEAM"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        let response = Schema::build(TestQuery, EmptyMutation, EmptySubscription)
            .data(claims)
            .finish()
            .execute(Request::new(
                "{ leaveRequest { viewerMayApprove pendingApprovalStepId } }",
            ))
            .await;
        assert!(response.errors.is_empty(), "unexpected response: {response:?}");
        assert_eq!(
            response.data.into_json().expect("GraphQL JSON"),
            serde_json::json!({
                "leaveRequest": {
                    "viewerMayApprove": false,
                    "pendingApprovalStepId": null
                }
            })
        );
    }
}
