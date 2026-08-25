//! Authorization boundary for managed attendance records.

use async_graphql::{Context, Result};
use kabipay_common::{
    client_data_scope::{
        resolve_employee_scope_filter_with_connection, resolve_viewer_employee_with_connection,
        EmployeeScopeFilter,
    },
    context::{ClientClaims, ClientViewerEmployee, ScopeType, PERM_ATTENDANCE_REGULARIZE},
    subgraph::require_client_claims,
    KabiPayError,
};
use kabipay_db_entities::tenant::{
    d0007_employee_core::employee, d0010_time_shift_roster::attendance,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

const ATTENDANCE_MANAGEMENT_ACCESS_DENIED: &str = "attendance management access denied";

fn access_denied() -> async_graphql::Error {
    KabiPayError::Forbidden(ATTENDANCE_MANAGEMENT_ACCESS_DENIED.into()).into_graphql()
}

pub fn require_regularizer(ctx: &Context<'_>) -> Result<()> {
    let claims = require_client_claims(ctx)?;
    if claims.can_regularize_attendance_records() {
        Ok(())
    } else {
        Err(access_denied())
    }
}

fn require_explicit_attendance_scope(claims: &ClientClaims) -> Result<ScopeType> {
    claims
        .explicit_scope_for_permission(PERM_ATTENDANCE_REGULARIZE)
        .ok_or_else(access_denied)
}

fn assert_target_allowed_by_filter(
    filter: &EmployeeScopeFilter,
    target_employee_id: Uuid,
) -> Result<()> {
    if filter.allows_employee(target_employee_id) {
        Ok(())
    } else {
        Err(access_denied())
    }
}

fn assert_target_exists(target_exists: bool) -> Result<()> {
    if target_exists {
        Ok(())
    } else {
        Err(access_denied())
    }
}

#[allow(async_fn_in_trait)]
trait AttendanceManagementRepository {
    async fn resolve_viewer(
        &self,
        ctx: &Context<'_>,
        tenant_id: Uuid,
    ) -> Result<Option<ClientViewerEmployee>>;

    async fn resolve_scope_filter(
        &self,
        tenant_id: Uuid,
        scope: ScopeType,
        viewer: Option<ClientViewerEmployee>,
    ) -> Result<EmployeeScopeFilter>;

    async fn attendance_by_id(
        &self,
        tenant_id: Uuid,
        attendance_id: Uuid,
    ) -> Result<Option<attendance::Model>>;

    async fn target_exists(&self, tenant_id: Uuid, target_employee_id: Uuid) -> Result<bool>;
}

struct SeaOrmAttendanceManagementRepository<'a, C>
where
    C: ConnectionTrait + Sync,
{
    db: &'a C,
}

impl<C> AttendanceManagementRepository for SeaOrmAttendanceManagementRepository<'_, C>
where
    C: ConnectionTrait + Sync,
{
    async fn resolve_viewer(
        &self,
        ctx: &Context<'_>,
        tenant_id: Uuid,
    ) -> Result<Option<ClientViewerEmployee>> {
        resolve_viewer_employee_with_connection(ctx, self.db, tenant_id).await
    }

    async fn resolve_scope_filter(
        &self,
        tenant_id: Uuid,
        scope: ScopeType,
        viewer: Option<ClientViewerEmployee>,
    ) -> Result<EmployeeScopeFilter> {
        resolve_employee_scope_filter_with_connection(self.db, tenant_id, scope, viewer)
            .await
            .map_err(KabiPayError::into_graphql)
    }

    async fn target_exists(&self, tenant_id: Uuid, target_employee_id: Uuid) -> Result<bool> {
        employee::Entity::find_by_id(target_employee_id)
            .filter(employee::Column::TenantId.eq(tenant_id))
            .filter(employee::Column::IsDeleted.eq(false))
            .one(self.db)
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)
            .map(|target| target.is_some())
    }

    async fn attendance_by_id(
        &self,
        tenant_id: Uuid,
        attendance_id: Uuid,
    ) -> Result<Option<attendance::Model>> {
        attendance::Entity::find_by_id(attendance_id)
            .filter(attendance::Column::TenantId.eq(tenant_id))
            .one(self.db)
            .await
            .map_err(KabiPayError::from)
            .map_err(KabiPayError::into_graphql)
    }
}

async fn scope_filter_with<R: AttendanceManagementRepository>(
    ctx: &Context<'_>,
    repository: &R,
    tenant_id: Uuid,
) -> Result<EmployeeScopeFilter> {
    require_regularizer(ctx)?;
    let claims = require_client_claims(ctx)?;
    let scope = require_explicit_attendance_scope(claims)?;
    let viewer = match scope {
        ScopeType::All => None,
        _ => repository.resolve_viewer(ctx, tenant_id).await?,
    };
    let filter = repository
        .resolve_scope_filter(tenant_id, scope, viewer)
        .await?;
    if matches!(filter, EmployeeScopeFilter::Empty) {
        return Err(access_denied());
    }
    Ok(filter)
}

#[cfg(test)]
async fn assert_target_in_scope_with<R: AttendanceManagementRepository>(
    ctx: &Context<'_>,
    repository: &R,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()> {
    let filter = scope_filter_with(ctx, repository, tenant_id).await?;
    assert_target_allowed_by_filter(&filter, target_employee_id)?;
    assert_target_exists(
        repository
            .target_exists(tenant_id, target_employee_id)
            .await?,
    )
}

async fn assert_target_in_resolved_scope_with<R: AttendanceManagementRepository>(
    repository: &R,
    tenant_id: Uuid,
    filter: &EmployeeScopeFilter,
    target_employee_id: Uuid,
) -> Result<()> {
    assert_target_allowed_by_filter(filter, target_employee_id)?;
    let target_exists = repository.target_exists(tenant_id, target_employee_id).await?;
    assert_target_exists(target_exists)
}

async fn attendance_target_in_scope_with<R: AttendanceManagementRepository>(
    ctx: &Context<'_>,
    repository: &R,
    tenant_id: Uuid,
    attendance_id: Uuid,
) -> Result<attendance::Model> {
    let filter = scope_filter_with(ctx, repository, tenant_id).await?;
    let attendance = repository
        .attendance_by_id(tenant_id, attendance_id)
        .await?
        .ok_or_else(access_denied)?;
    assert_target_in_resolved_scope_with(
        repository,
        tenant_id,
        &filter,
        attendance.employee_id,
    )
    .await?;
    Ok(attendance)
}

pub async fn scope_filter(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
) -> Result<EmployeeScopeFilter> {
    scope_filter_with_connection(ctx, db, tenant_id).await
}

/// Transaction-compatible managed attendance scope resolution for Task 4B.
///
/// The public [`scope_filter`] wrapper remains available to existing callers.
pub(crate) async fn scope_filter_with_connection<C>(
    ctx: &Context<'_>,
    db: &C,
    tenant_id: Uuid,
) -> Result<EmployeeScopeFilter>
where
    C: ConnectionTrait + Sync,
{
    scope_filter_with(ctx, &SeaOrmAttendanceManagementRepository { db }, tenant_id).await
}

/// Preserved Task 2 authorization entry point for mutation consumers.
#[allow(dead_code)]
pub async fn assert_target_in_scope(
    ctx: &Context<'_>,
    db: &DatabaseConnection,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()> {
    assert_target_in_scope_with_connection(ctx, db, tenant_id, target_employee_id).await
}

/// Transaction-compatible managed attendance authorization for Task 4B.
///
/// It preserves the existing permission, scope, and target-existence order.
pub(crate) async fn assert_target_in_scope_with_connection<C>(
    ctx: &Context<'_>,
    db: &C,
    tenant_id: Uuid,
    target_employee_id: Uuid,
) -> Result<()>
where
    C: ConnectionTrait + Sync,
{
    let filter = scope_filter_with_connection(ctx, db, tenant_id).await?;
    assert_target_in_resolved_scope_with_connection(db, tenant_id, &filter, target_employee_id)
        .await
}

pub async fn assert_target_in_resolved_scope(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    filter: &EmployeeScopeFilter,
    target_employee_id: Uuid,
) -> Result<()> {
    assert_target_in_resolved_scope_with_connection(db, tenant_id, filter, target_employee_id).await
}

/// Transaction-compatible target check for an already resolved attendance scope.
pub(crate) async fn assert_target_in_resolved_scope_with_connection<C>(
    db: &C,
    tenant_id: Uuid,
    filter: &EmployeeScopeFilter,
    target_employee_id: Uuid,
) -> Result<()>
where
    C: ConnectionTrait + Sync,
{
    assert_target_in_resolved_scope_with(
        &SeaOrmAttendanceManagementRepository { db },
        tenant_id,
        filter,
        target_employee_id,
    )
    .await
}

/// Resolves managed-attendance permission and explicit scope before revealing
/// whether a stored attendance row exists, then authorizes that row's employee.
pub(crate) async fn attendance_target_in_scope_with_connection<C>(
    ctx: &Context<'_>,
    db: &C,
    tenant_id: Uuid,
    attendance_id: Uuid,
) -> Result<attendance::Model>
where
    C: ConnectionTrait + Sync,
{
    attendance_target_in_scope_with(
        ctx,
        &SeaOrmAttendanceManagementRepository { db },
        tenant_id,
        attendance_id,
    )
    .await
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Object, Request, Schema};
    use chrono::{NaiveDate, NaiveTime, TimeZone, Utc};
    use kabipay_common::context::{CLIENT_JWT_ISSUER, PERM_ATTENDANCE_REGULARIZE};
    use sea_orm::DatabaseTransaction;
    use serde_json::json;
    use std::{collections::HashMap, sync::{Arc, Mutex}};

    const TENANT_ID: Uuid = Uuid::from_u128(1);
    const MANAGER_ID: Uuid = Uuid::from_u128(2);
    const DIRECT_REPORT_ID: Uuid = Uuid::from_u128(3);
    const OUTSIDE_EMPLOYEE_ID: Uuid = Uuid::from_u128(4);
    const ATTENDANCE_ID: Uuid = Uuid::from_u128(5);
    const DENIAL_MESSAGE_SUFFIX: &str = "attendance management access denied";

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum Operation {
        ResolveViewer,
        ResolveScopeFilter,
        AttendanceLookup,
        TargetExists,
    }

    struct FakeRepository {
        viewer: Option<ClientViewerEmployee>,
        filter: EmployeeScopeFilter,
        attendance: Option<attendance::Model>,
        target_exists: bool,
        operations: Mutex<Vec<Operation>>,
    }

    impl FakeRepository {
        fn operations(&self) -> Vec<Operation> {
            self.operations.lock().expect("test operation lock poisoned").clone()
        }
    }

    impl AttendanceManagementRepository for FakeRepository {
        async fn resolve_viewer(
            &self,
            _ctx: &Context<'_>,
            _tenant_id: Uuid,
        ) -> Result<Option<ClientViewerEmployee>> {
            self.operations
                .lock()
                .expect("test operation lock poisoned")
                .push(Operation::ResolveViewer);
            Ok(self.viewer)
        }

        async fn resolve_scope_filter(
            &self,
            _tenant_id: Uuid,
            _scope: ScopeType,
            _viewer: Option<ClientViewerEmployee>,
        ) -> Result<EmployeeScopeFilter> {
            self.operations
                .lock()
                .expect("test operation lock poisoned")
                .push(Operation::ResolveScopeFilter);
            Ok(self.filter.clone())
        }

        async fn target_exists(
            &self,
            _tenant_id: Uuid,
            _target_employee_id: Uuid,
        ) -> Result<bool> {
            self.operations
                .lock()
                .expect("test operation lock poisoned")
                .push(Operation::TargetExists);
            Ok(self.target_exists)
        }

        async fn attendance_by_id(
            &self,
            _tenant_id: Uuid,
            _attendance_id: Uuid,
        ) -> Result<Option<attendance::Model>> {
            self.operations
                .lock()
                .expect("test operation lock poisoned")
                .push(Operation::AttendanceLookup);
            Ok(self.attendance.clone())
        }
    }

    struct ManagementBoundaryQuery;

    #[Object]
    impl ManagementBoundaryQuery {
        async fn public_scope_filter(&self, ctx: &Context<'_>) -> Result<bool> {
            let db = ctx.data::<DatabaseConnection>()?;
            let tenant_id = *ctx.data::<Uuid>()?;
            let filter = scope_filter(ctx, db, tenant_id).await?;
            Ok(filter.allows_employee(DIRECT_REPORT_ID))
        }

        async fn public_target(&self, ctx: &Context<'_>) -> Result<bool> {
            let db = ctx.data::<DatabaseConnection>()?;
            let tenant_id = *ctx.data::<Uuid>()?;
            assert_target_in_scope(ctx, db, tenant_id, DIRECT_REPORT_ID).await?;
            Ok(true)
        }

        async fn seam_target(&self, ctx: &Context<'_>, target_employee_id: String) -> Result<bool> {
            let repository = ctx.data::<Arc<FakeRepository>>()?;
            let tenant_id = *ctx.data::<Uuid>()?;
            let target_employee_id = Uuid::parse_str(&target_employee_id)
                .map_err(|_| KabiPayError::Validation("invalid target employee id".into()).into_graphql())?;
            assert_target_in_scope_with(ctx, repository.as_ref(), tenant_id, target_employee_id).await?;
            Ok(true)
        }

        async fn seam_stored_attendance(&self, ctx: &Context<'_>) -> Result<bool> {
            let repository = ctx.data::<Arc<FakeRepository>>()?;
            let tenant_id = *ctx.data::<Uuid>()?;
            attendance_target_in_scope_with(ctx, repository.as_ref(), tenant_id, ATTENDANCE_ID)
                .await?;
            Ok(true)
        }
    }

    fn claims(scope: Option<&str>) -> ClientClaims {
        let permission_scopes = scope
            .map(|scope| {
                HashMap::from([(
                    PERM_ATTENDANCE_REGULARIZE.to_string(),
                    scope.to_string(),
                )])
            })
            .unwrap_or_default();
        ClientClaims {
            sub: Uuid::from_u128(99),
            iss: CLIENT_JWT_ISSUER.to_string(),
            exp: 0,
            iat: 0,
            tenant_id: TENANT_ID,
            email: String::new(),
            employee_id: Some(MANAGER_ID),
            must_change_password: false,
            roles: vec![],
            permissions: vec![PERM_ATTENDANCE_REGULARIZE.to_string()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    fn fake_repository(
        viewer: Option<ClientViewerEmployee>,
        filter: EmployeeScopeFilter,
        target_exists: bool,
    ) -> Arc<FakeRepository> {
        Arc::new(FakeRepository {
            viewer,
            filter,
            attendance: None,
            target_exists,
            operations: Mutex::new(vec![]),
        })
    }

    fn stored_attendance(employee_id: Uuid) -> attendance::Model {
        attendance::Model {
            id: ATTENDANCE_ID,
            tenant_id: TENANT_ID,
            employee_id,
            shift_id: None,
            work_date: NaiveDate::from_ymd_opt(2026, 8, 20).expect("valid test date"),
            check_in_time: Some(NaiveTime::from_hms_opt(9, 0, 0).expect("valid test time")),
            check_out_time: Some(
                NaiveTime::from_hms_opt(17, 0, 0).expect("valid test time"),
            ),
            check_in_at: None,
            check_out_at: None,
            check_in_lat: None,
            check_in_lng: None,
            check_out_lat: None,
            check_out_lng: None,
            source: Some("MANUAL".into()),
            status: Some("PRESENT".into()),
            regularization_status: None,
            biometric_ref: None,
            overtime_hours: None,
            late_minutes: None,
            early_exit_minutes: None,
            created_at: Utc
                .with_ymd_and_hms(2026, 8, 20, 9, 0, 0)
                .single()
                .expect("valid test timestamp"),
            updated_at: Utc
                .with_ymd_and_hms(2026, 8, 20, 9, 0, 0)
                .single()
                .expect("valid test timestamp"),
        }
    }

    fn fake_repository_with_attendance(
        viewer: Option<ClientViewerEmployee>,
        filter: EmployeeScopeFilter,
        attendance: attendance::Model,
        target_exists: bool,
    ) -> Arc<FakeRepository> {
        Arc::new(FakeRepository {
            viewer,
            filter,
            attendance: Some(attendance),
            target_exists,
            operations: Mutex::new(vec![]),
        })
    }

    async fn execute_public(claims: ClientClaims, query: &str) -> async_graphql::Response {
        Schema::build(ManagementBoundaryQuery, EmptyMutation, EmptySubscription)
            .data(claims)
            .data(DatabaseConnection::Disconnected)
            .data(TENANT_ID)
            .data(fake_repository(None, EmployeeScopeFilter::Empty, false))
            .finish()
            .execute(Request::new(query))
            .await
    }

    async fn execute_with_fake(
        claims: ClientClaims,
        repository: Arc<FakeRepository>,
        target_employee_id: Uuid,
    ) -> (async_graphql::Response, Arc<FakeRepository>) {
        let response = Schema::build(ManagementBoundaryQuery, EmptyMutation, EmptySubscription)
            .data(claims)
            .data(DatabaseConnection::Disconnected)
            .data(TENANT_ID)
            .data(repository.clone())
            .finish()
            .execute(Request::new(format!(
                "{{ seamTarget(targetEmployeeId: \"{target_employee_id}\") }}"
            )))
            .await;
        (response, repository)
    }

    async fn execute_stored_with_fake(
        claims: ClientClaims,
        repository: Arc<FakeRepository>,
    ) -> (async_graphql::Response, Arc<FakeRepository>) {
        let response = Schema::build(ManagementBoundaryQuery, EmptyMutation, EmptySubscription)
            .data(claims)
            .data(DatabaseConnection::Disconnected)
            .data(TENANT_ID)
            .data(repository.clone())
            .finish()
            .execute(Request::new("{ seamStoredAttendance }"))
            .await;
        (response, repository)
    }

    fn assert_allowed(response: &async_graphql::Response, field: &str) {
        assert!(response.errors.is_empty(), "unexpected errors: {:?}", response.errors);
        assert_eq!(response.data.clone().into_json().unwrap(), json!({field: true}));
    }

    fn assert_denied(response: &async_graphql::Response) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {:?}", response);
        assert!(response.errors[0].message.ends_with(DENIAL_MESSAGE_SUFFIX));
    }

    #[test]
    fn database_transaction_compiles_with_the_managed_authorization_seam() {
        async fn invoke_transaction_authorization(
            ctx: &Context<'_>,
            transaction: &DatabaseTransaction,
            tenant_id: Uuid,
            target_employee_id: Uuid,
        ) -> Result<()> {
            assert_target_in_scope_with_connection(ctx, transaction, tenant_id, target_employee_id)
                .await
        }

        let _ = invoke_transaction_authorization;
    }

    #[tokio::test]
    async fn attendance_management_resolves_scope_before_stored_attendance_lookup() {
        let repository = fake_repository_with_attendance(
            Some(ClientViewerEmployee {
                employee_id: MANAGER_ID,
                department_id: None,
            }),
            EmployeeScopeFilter::EmployeeIds(vec![MANAGER_ID, DIRECT_REPORT_ID]),
            stored_attendance(DIRECT_REPORT_ID),
            true,
        );

        let (response, repository) =
            execute_stored_with_fake(claims(Some("TEAM")), repository).await;

        assert_allowed(&response, "seamStoredAttendance");
        assert_eq!(
            repository.operations(),
            vec![
                Operation::ResolveViewer,
                Operation::ResolveScopeFilter,
                Operation::AttendanceLookup,
                Operation::TargetExists,
            ]
        );
    }

    #[tokio::test]
    async fn attendance_management_public_scope_filter_allows_explicit_all_without_a_database_query() {
        let response = execute_public(claims(Some("ALL")), "{ publicScopeFilter }").await;

        assert_allowed(&response, "publicScopeFilter");
    }

    #[tokio::test]
    async fn attendance_management_public_target_denies_missing_scope_before_target_query() {
        let response = execute_public(claims(None), "{ publicTarget }").await;

        assert_denied(&response);
    }

    #[tokio::test]
    async fn attendance_management_public_target_denies_invalid_scope_before_target_query() {
        let response = execute_public(claims(Some("INVALID")), "{ publicTarget }").await;

        assert_denied(&response);
    }

    #[tokio::test]
    async fn attendance_management_team_scope_allows_self_and_direct_report_in_production_orchestration() {
        let viewer = ClientViewerEmployee {
            employee_id: MANAGER_ID,
            department_id: None,
        };
        let self_repository = fake_repository(
            Some(viewer),
            EmployeeScopeFilter::EmployeeIds(vec![MANAGER_ID, DIRECT_REPORT_ID]),
            true,
        );
        let (self_response, self_repository) =
            execute_with_fake(claims(Some("TEAM")), self_repository, MANAGER_ID).await;
        assert_allowed(&self_response, "seamTarget");
        assert_eq!(
            self_repository.operations(),
            vec![
                Operation::ResolveViewer,
                Operation::ResolveScopeFilter,
                Operation::TargetExists,
            ]
        );

        let direct_repository = fake_repository(
            Some(viewer),
            EmployeeScopeFilter::EmployeeIds(vec![MANAGER_ID, DIRECT_REPORT_ID]),
            true,
        );
        let (direct_response, direct_repository) =
            execute_with_fake(claims(Some("TEAM")), direct_repository, DIRECT_REPORT_ID).await;
        assert_allowed(&direct_response, "seamTarget");
        assert_eq!(
            direct_repository.operations(),
            vec![
                Operation::ResolveViewer,
                Operation::ResolveScopeFilter,
                Operation::TargetExists,
            ]
        );
    }

    #[tokio::test]
    async fn attendance_management_empty_scope_denies_before_target_lookup() {
        let repository = fake_repository(None, EmployeeScopeFilter::Empty, true);
        let (response, repository) =
            execute_with_fake(claims(Some("TEAM")), repository, DIRECT_REPORT_ID).await;

        assert_denied(&response);
        assert_eq!(
            repository.operations(),
            vec![Operation::ResolveViewer, Operation::ResolveScopeFilter]
        );
    }

    #[tokio::test]
    async fn attendance_management_unknown_and_out_of_scope_targets_are_denied() {
        let unknown_repository = fake_repository(None, EmployeeScopeFilter::Unrestricted, false);
        let (unknown_response, unknown_repository) =
            execute_with_fake(claims(Some("ALL")), unknown_repository, DIRECT_REPORT_ID).await;
        assert_denied(&unknown_response);
        assert_eq!(
            unknown_repository.operations(),
            vec![Operation::ResolveScopeFilter, Operation::TargetExists]
        );

        let out_of_scope_repository = fake_repository(
            Some(ClientViewerEmployee {
                employee_id: MANAGER_ID,
                department_id: None,
            }),
            EmployeeScopeFilter::EmployeeIds(vec![MANAGER_ID, DIRECT_REPORT_ID]),
            true,
        );
        let (out_of_scope_response, out_of_scope_repository) = execute_with_fake(
            claims(Some("TEAM")),
            out_of_scope_repository,
            OUTSIDE_EMPLOYEE_ID,
        )
        .await;
        assert_denied(&out_of_scope_response);
        assert_eq!(
            out_of_scope_repository.operations(),
            vec![Operation::ResolveViewer, Operation::ResolveScopeFilter]
        );
    }
}
