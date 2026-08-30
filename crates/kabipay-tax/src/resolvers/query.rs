//! Root query resolvers for kabipay-tax.

use async_graphql::{Context, Object, Result, ID};
use kabipay_common::{
    client_data_scope::{
        data_scope_from_claims, resolve_employee_scope_filter, resolve_viewer_employee,
        EmployeeScopeFilter,
    },
    context::{ClientClaims, ScopeType, PERM_TAX_READ},
    subgraph::{require_tenant_id, resolve_client_employee_id, tenant_db},
    KabiPayError, KabiPayResult,
};
use uuid::Uuid;

use crate::resolvers::types::{
    TaxComputationDto, TaxConfigurationVersionDto, TaxProofLineDto, TaxSectionDefinitionDto,
    TaxSlabDto,
};
use crate::services::tax_service;

fn parse_uuid(id: &ID, field: &'static str) -> Result<Uuid> {
    Uuid::parse_str(id.as_str())
        .map_err(|e| KabiPayError::Validation(format!("invalid {field}: {e}")).into_graphql())
}

fn tax_read_scope_from_claims(claims: Option<&ClientClaims>) -> KabiPayResult<ScopeType> {
    data_scope_from_claims(claims, PERM_TAX_READ)
}

fn tax_read_scope(ctx: &Context<'_>) -> Result<ScopeType> {
    tax_read_scope_from_claims(ctx.data_opt::<ClientClaims>()).map_err(KabiPayError::into_graphql)
}

fn require_tax_target_scope(
    filter: &EmployeeScopeFilter,
    target_employee_id: Uuid,
) -> KabiPayResult<()> {
    if filter.allows_employee(target_employee_id) {
        return Ok(());
    }
    Err(KabiPayError::Forbidden(
        "tax:read scope does not include target employee".into(),
    ))
}

async fn load_scoped_employee_target_with<
    T,
    ResolveJwtEmployee,
    ResolveJwtEmployeeFuture,
    ResolveViewer,
    ResolveViewerFuture,
    ResolveScope,
    ResolveScopeFuture,
    Load,
    LoadFuture,
>(
    requested_employee_id: Option<Uuid>,
    scope: ScopeType,
    resolve_jwt_employee: ResolveJwtEmployee,
    resolve_viewer: ResolveViewer,
    resolve_scope: ResolveScope,
    load: Load,
) -> Result<T>
where
    ResolveJwtEmployee: FnOnce() -> ResolveJwtEmployeeFuture,
    ResolveJwtEmployeeFuture: std::future::Future<Output = Result<Uuid>>,
    ResolveViewer: FnOnce() -> ResolveViewerFuture,
    ResolveViewerFuture: std::future::Future<
        Output = Result<Option<kabipay_common::context::ClientViewerEmployee>>,
    >,
    ResolveScope: FnOnce(
        ScopeType,
        Option<kabipay_common::context::ClientViewerEmployee>,
    ) -> ResolveScopeFuture,
    ResolveScopeFuture: std::future::Future<Output = Result<EmployeeScopeFilter>>,
    Load: FnOnce(Uuid) -> LoadFuture,
    LoadFuture: std::future::Future<Output = Result<T>>,
{
    let target_employee_id = match requested_employee_id {
        Some(employee_id) => employee_id,
        None => resolve_jwt_employee().await?,
    };
    let viewer = if scope == ScopeType::All {
        None
    } else {
        resolve_viewer().await?
    };
    let filter = resolve_scope(scope, viewer).await?;
    require_tax_target_scope(&filter, target_employee_id).map_err(KabiPayError::into_graphql)?;
    load(target_employee_id).await
}

pub struct QueryRoot;

#[Object]
impl QueryRoot {
    async fn tax_health(&self) -> &'static str {
        "ok"
    }

    /// Tax configuration versions configured for this tenant.
    async fn tax_configurations(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 20)] limit: u64,
    ) -> Result<Vec<TaxConfigurationVersionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        tax_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = tax_service::list_configurations(&db, tenant_id, active_only, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(TaxConfigurationVersionDto::from)
            .collect())
    }

    /// Deduction sections catalogue (**`tax_proof_line.section_code`**) — admin-maintained labels & caps.
    async fn tax_section_definitions(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = true)] active_only: bool,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TaxSectionDefinitionDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        tax_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = tax_service::list_tax_section_definitions(&db, tenant_id, active_only, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows
            .into_iter()
            .map(TaxSectionDefinitionDto::from)
            .collect())
    }

    /// Tax slabs for this tenant (filter by fiscal_year server-side later).
    async fn tax_slabs(
        &self,
        ctx: &Context<'_>,
        #[graphql(default = 100)] limit: u64,
    ) -> Result<Vec<TaxSlabDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        tax_read_scope(ctx)?;
        let db = tenant_db(ctx, tenant_id).await?;
        let rows = tax_service::list_slabs(&db, tenant_id, limit)
            .await
            .map_err(KabiPayError::into_graphql)?;
        Ok(rows.into_iter().map(TaxSlabDto::from).collect())
    }

    /// Stored per-employee tax computation / declaration rows for a fiscal period.
    /// Omit `employeeId` to use the signed-in user’s employee record.
    async fn tax_computations(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        #[graphql(default = 20)] limit: u64,
    ) -> Result<Vec<TaxComputationDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = tax_read_scope(ctx)?;
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let db = tenant_db(ctx, tenant_id).await?;
        let jwt_db = &db;
        let viewer_db = &db;
        let scope_db = &db;
        let load_db = &db;
        load_scoped_employee_target_with(
            requested_employee_id,
            scope,
            || async move {
                resolve_client_employee_id(ctx, jwt_db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            || async move { resolve_viewer_employee(ctx, viewer_db, tenant_id).await },
            |resolved_scope, viewer| async move {
                resolve_employee_scope_filter(scope_db, tenant_id, resolved_scope, viewer)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            |target_employee_id| async move {
                tax_service::list_computations(load_db, tenant_id, target_employee_id, limit)
                    .await
                    .map_err(KabiPayError::into_graphql)
                    .map(|rows| rows.into_iter().map(TaxComputationDto::from).collect())
            },
        )
        .await
    }

    /// Deduction proof lines (declared vs actual) for an employee. Omit `employeeId` for self;
    /// viewing another employee requires exact `tax:read` target scope.
    async fn tax_proof_lines(
        &self,
        ctx: &Context<'_>,
        employee_id: Option<ID>,
        tax_config_version_id: Option<ID>,
        fiscal_year: Option<i32>,
    ) -> Result<Vec<TaxProofLineDto>> {
        let tenant_id = require_tenant_id(ctx)?;
        let scope = tax_read_scope(ctx)?;
        let requested_employee_id = employee_id
            .as_ref()
            .map(|id| parse_uuid(id, "employeeId"))
            .transpose()?;
        let db = tenant_db(ctx, tenant_id).await?;
        let cfg = tax_config_version_id
            .as_ref()
            .map(|id| parse_uuid(id, "taxConfigVersionId"))
            .transpose()?;
        let jwt_db = &db;
        let viewer_db = &db;
        let scope_db = &db;
        let load_db = &db;
        load_scoped_employee_target_with(
            requested_employee_id,
            scope,
            || async move {
                resolve_client_employee_id(ctx, jwt_db, tenant_id)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            || async move { resolve_viewer_employee(ctx, viewer_db, tenant_id).await },
            |resolved_scope, viewer| async move {
                resolve_employee_scope_filter(scope_db, tenant_id, resolved_scope, viewer)
                    .await
                    .map_err(KabiPayError::into_graphql)
            },
            |target_employee_id| async move {
                tax_service::list_tax_proof_lines(
                    load_db,
                    tenant_id,
                    target_employee_id,
                    cfg,
                    fiscal_year,
                )
                .await
                .map_err(KabiPayError::into_graphql)
                .map(|rows| rows.into_iter().map(TaxProofLineDto::from).collect())
            },
        )
        .await
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptyMutation, EmptySubscription, Request, Schema};
    use kabipay_common::client_data_scope::{
        resolve_employee_scope_filter_with_connection, EmployeeScopeFilter,
    };
    use kabipay_common::context::{
        ClientClaims, ScopeType, CLIENT_JWT_ISSUER, EMPLOYMENT_STATUS_ACTIVE,
        EMPLOYMENT_STATUS_PROBATION, PERM_TAX_APPROVE, PERM_TAX_READ,
    };
    use kabipay_common::subgraph::TenantId;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DatabaseConnection, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult,
        ProxyRow, Statement,
    };
    use std::cell::RefCell;
    use std::collections::{BTreeMap, HashMap};
    use std::sync::{Arc, Mutex};

    fn claims(permission: &str, scope: Option<&str>) -> ClientClaims {
        let permission_scopes = scope
            .map(|scope| HashMap::from([(permission.to_string(), scope.to_string())]))
            .unwrap_or_default();
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
            permissions: vec![permission.into()],
            permission_scopes,
            resource_scopes: HashMap::new(),
        }
    }

    fn claims_without_permissions() -> ClientClaims {
        let mut claims = claims("unrelated:read", Some("ALL"));
        claims.permissions.clear();
        claims.permission_scopes.clear();
        claims
    }

    async fn execute_query(claims: ClientClaims, query: &str) -> async_graphql::Response {
        let tenant_id = claims.tenant_id;
        Schema::build(QueryRoot, EmptyMutation, EmptySubscription)
            .data(TenantId(tenant_id))
            .data(claims)
            .finish()
            .execute(Request::new(query))
            .await
    }

    fn assert_permission_denied_before_db(
        response: &async_graphql::Response,
        expected_message: &str,
    ) {
        assert_eq!(response.errors.len(), 1, "unexpected response: {response:?}");
        let message = &response.errors[0].message;
        assert!(
            message.contains(expected_message),
            "unexpected denial: {message}"
        );
        assert!(!message.contains("TenantDbCache"));
        assert!(!message.contains("database"));
    }

    #[derive(Clone, Debug, PartialEq, Eq)]
    enum TargetBoundaryOperation {
        ResolveJwtEmployee,
        ResolveViewer,
        ResolveScope(ScopeType, Option<Uuid>),
        Load(Uuid),
    }

    #[derive(Debug)]
    struct ScopeProxy {
        rows: Vec<ProxyRow>,
        statements: Arc<Mutex<Vec<Statement>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for ScopeProxy {
        async fn query(&self, statement: Statement) -> std::result::Result<Vec<ProxyRow>, DbErr> {
            self.statements
                .lock()
                .expect("scope statement recorder lock")
                .push(statement);
            Ok(self.rows.clone())
        }

        async fn execute(
            &self,
            _statement: Statement,
        ) -> std::result::Result<ProxyExecResult, DbErr> {
            Err(DbErr::Custom(
                "scope resolver unexpectedly executed a write statement".into(),
            ))
        }
    }

    struct TargetBoundaryFixture {
        tenant_id: Uuid,
        jwt_employee_id: Option<Uuid>,
        viewer: Option<kabipay_common::context::ClientViewerEmployee>,
        scope_db: DatabaseConnection,
        scope_statements: Arc<Mutex<Vec<Statement>>>,
        operations: RefCell<Vec<TargetBoundaryOperation>>,
    }

    impl TargetBoundaryFixture {
        async fn new(
            jwt_employee_id: Option<Uuid>,
            viewer: Option<kabipay_common::context::ClientViewerEmployee>,
            team_employee_ids: Vec<Uuid>,
        ) -> Self {
            let rows: Vec<ProxyRow> = team_employee_ids
                .into_iter()
                .map(|employee_id| {
                    ProxyRow::new(BTreeMap::from([("id".into(), employee_id.into())]))
                })
                .collect();
            let scope_statements = Arc::new(Mutex::new(Vec::new()));
            let scope_db = Database::connect_proxy(
                DbBackend::Postgres,
                Arc::new(Box::new(ScopeProxy {
                    rows,
                    statements: Arc::clone(&scope_statements),
                })),
            )
            .await
            .expect("PostgreSQL scope proxy connection");
            Self {
                tenant_id: Uuid::new_v4(),
                jwt_employee_id,
                viewer,
                scope_db,
                scope_statements,
                operations: RefCell::new(Vec::new()),
            }
        }

        fn operations(&self) -> Vec<TargetBoundaryOperation> {
            self.operations.borrow().clone()
        }

        async fn resolve_scope(
            &self,
            scope: ScopeType,
            viewer: Option<kabipay_common::context::ClientViewerEmployee>,
        ) -> Result<EmployeeScopeFilter> {
            self.operations.borrow_mut().push(
                TargetBoundaryOperation::ResolveScope(scope, viewer.map(|v| v.employee_id)),
            );
            resolve_employee_scope_filter_with_connection(
                &self.scope_db,
                self.tenant_id,
                scope,
                viewer,
            )
            .await
            .map_err(KabiPayError::into_graphql)
        }

        fn scope_statements(&self) -> Vec<Statement> {
            self.scope_statements
                .lock()
                .expect("scope statement recorder lock")
                .clone()
        }

        async fn execute(
            &self,
            requested_employee_id: Option<Uuid>,
            scope: ScopeType,
        ) -> Result<Uuid> {
            load_scoped_employee_target_with(
                requested_employee_id,
                scope,
                || async {
                    self.operations
                        .borrow_mut()
                        .push(TargetBoundaryOperation::ResolveJwtEmployee);
                    self.jwt_employee_id.ok_or_else(|| {
                        KabiPayError::Forbidden("JWT-bound employee required".into()).into_graphql()
                    })
                },
                || async {
                    self.operations
                        .borrow_mut()
                        .push(TargetBoundaryOperation::ResolveViewer);
                    Ok(self.viewer)
                },
                |scope, viewer| async move { self.resolve_scope(scope, viewer).await },
                |target_employee_id| async move {
                    self.operations
                        .borrow_mut()
                        .push(TargetBoundaryOperation::Load(target_employee_id));
                    Ok(target_employee_id)
                },
            )
            .await
        }
    }

    #[tokio::test]
    async fn tax_boundary_explicit_all_succeeds_without_linked_employee() {
        let target_id = Uuid::new_v4();
        let fixture = TargetBoundaryFixture::new(None, None, Vec::new()).await;

        assert_eq!(
            fixture
                .execute(Some(target_id), ScopeType::All)
                .await
                .expect("explicit ALL target reaches tax service"),
            target_id
        );
        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveScope(ScopeType::All, None),
                TargetBoundaryOperation::Load(target_id),
            ]
        );
    }

    #[tokio::test]
    async fn tax_boundary_omitted_target_requires_jwt_employee() {
        let fixture = TargetBoundaryFixture::new(None, None, Vec::new()).await;

        let error = fixture
            .execute(None, ScopeType::All)
            .await
            .expect_err("omitted target must require JWT employee binding");

        assert!(error.message.contains("JWT-bound employee required"));
        assert_eq!(
            fixture.operations(),
            vec![TargetBoundaryOperation::ResolveJwtEmployee]
        );
    }

    #[tokio::test]
    async fn tax_boundary_omitted_target_uses_jwt_employee_then_loads() {
        let jwt_employee_id = Uuid::new_v4();
        let fixture =
            TargetBoundaryFixture::new(Some(jwt_employee_id), None, Vec::new()).await;

        assert_eq!(
            fixture
                .execute(None, ScopeType::All)
                .await
                .expect("JWT-bound employee reaches tax service"),
            jwt_employee_id
        );
        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveJwtEmployee,
                TargetBoundaryOperation::ResolveScope(ScopeType::All, None),
                TargetBoundaryOperation::Load(jwt_employee_id),
            ]
        );
    }

    #[tokio::test]
    async fn tax_boundary_self_and_team_without_viewer_deny_before_service() {
        for scope in [ScopeType::Self_, ScopeType::Team] {
            let target_id = Uuid::new_v4();
            let fixture = TargetBoundaryFixture::new(None, None, Vec::new()).await;

            let error = fixture
                .execute(Some(target_id), scope)
                .await
                .expect_err("viewer-bound scope must deny without a viewer");

            assert!(error.message.contains("scope does not include target employee"));
            assert_eq!(
                fixture.operations(),
                vec![
                    TargetBoundaryOperation::ResolveViewer,
                    TargetBoundaryOperation::ResolveScope(scope, None),
                ]
            );
        }
    }

    #[tokio::test]
    async fn tax_boundary_team_uses_recursive_reporting_hierarchy() {
        let manager_id = Uuid::new_v4();
        let direct_report_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let fixture = TargetBoundaryFixture::new(
            None,
            Some(kabipay_common::context::ClientViewerEmployee {
                employee_id: manager_id,
                department_id: None,
            }),
            vec![manager_id, direct_report_id, descendant_id],
        )
        .await;
        let tenant_id = fixture.tenant_id;

        assert_eq!(
            fixture
                .execute(Some(descendant_id), ScopeType::Team)
                .await
                .expect("recursive TEAM descendant reaches tax service"),
            descendant_id
        );
        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveViewer,
                TargetBoundaryOperation::ResolveScope(ScopeType::Team, Some(manager_id)),
                TargetBoundaryOperation::Load(descendant_id),
            ]
        );
        let statements = fixture.scope_statements();
        assert_eq!(statements.len(), 1);
        let statement = &statements[0];
        assert_eq!(statement.db_backend, DbBackend::Postgres);
        assert!(statement.sql.contains("WITH RECURSIVE team"));
        assert!(statement.sql.contains("root.tenant_id = $1"));
        assert!(statement.sql.contains("child.tenant_id = $1"));
        assert_eq!(
            statement.values.as_ref().expect("bound TEAM query values").0,
            vec![
                tenant_id.into(),
                manager_id.into(),
                EMPLOYMENT_STATUS_ACTIVE.into(),
                EMPLOYMENT_STATUS_PROBATION.into(),
            ]
        );
    }

    #[tokio::test]
    async fn tax_boundary_out_of_scope_target_skips_service() {
        let manager_id = Uuid::new_v4();
        let direct_report_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();
        let fixture = TargetBoundaryFixture::new(
            None,
            Some(kabipay_common::context::ClientViewerEmployee {
                employee_id: manager_id,
                department_id: None,
            }),
            vec![manager_id, direct_report_id],
        )
        .await;
        let tenant_id = fixture.tenant_id;

        fixture
            .execute(Some(outside_id), ScopeType::Team)
            .await
            .expect_err("outside target must be rejected");

        assert_eq!(
            fixture.operations(),
            vec![
                TargetBoundaryOperation::ResolveViewer,
                TargetBoundaryOperation::ResolveScope(ScopeType::Team, Some(manager_id)),
            ]
        );
        let statements = fixture.scope_statements();
        assert_eq!(statements.len(), 1);
        assert_eq!(
            statements[0]
                .values
                .as_ref()
                .expect("bound TEAM query values")
                .0,
            vec![
                tenant_id.into(),
                manager_id.into(),
                EMPLOYMENT_STATUS_ACTIVE.into(),
                EMPLOYMENT_STATUS_PROBATION.into(),
            ]
        );
    }

    #[test]
    fn tax_read_gate_requires_its_own_valid_exact_scope() {
        for (wire_scope, expected) in [
            ("SELF", ScopeType::Self_),
            ("TEAM", ScopeType::Team),
            ("DEPARTMENT", ScopeType::Department),
            ("ALL", ScopeType::All),
        ] {
            assert_eq!(
                tax_read_scope_from_claims(Some(&claims(PERM_TAX_READ, Some(wire_scope))))
                    .expect("valid exact tax read scope"),
                expected
            );
        }

        for denied_claims in [
            claims(PERM_TAX_READ, None),
            claims(PERM_TAX_READ, Some("INVALID")),
            claims(PERM_TAX_APPROVE, Some("ALL")),
        ] {
            assert!(matches!(
                tax_read_scope_from_claims(Some(&denied_claims)),
                Err(KabiPayError::Forbidden(_))
            ));
        }
    }

    #[test]
    fn tax_target_scope_enforces_self_team_all_and_empty_filters() {
        let viewer_id = Uuid::new_v4();
        let descendant_id = Uuid::new_v4();
        let outside_id = Uuid::new_v4();
        let self_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id]);
        let team_filter = EmployeeScopeFilter::EmployeeIds(vec![viewer_id, descendant_id]);

        assert!(require_tax_target_scope(&self_filter, viewer_id).is_ok());
        assert!(matches!(
            require_tax_target_scope(&self_filter, descendant_id),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(require_tax_target_scope(&team_filter, descendant_id).is_ok());
        assert!(matches!(
            require_tax_target_scope(&team_filter, outside_id),
            Err(KabiPayError::Forbidden(_))
        ));
        assert!(require_tax_target_scope(&EmployeeScopeFilter::Unrestricted, outside_id).is_ok());
        assert!(matches!(
            require_tax_target_scope(&EmployeeScopeFilter::Empty, outside_id),
            Err(KabiPayError::Forbidden(_))
        ));
    }

    #[tokio::test]
    async fn explicit_tax_target_allows_all_without_viewer_and_denies_self_or_team() {
        let tenant_id = Uuid::new_v4();
        let target_id = Uuid::new_v4();

        let all = resolve_employee_scope_filter(
            &DatabaseConnection::Disconnected,
            tenant_id,
            ScopeType::All,
            None,
        )
        .await
        .expect("ALL scope does not need a viewer");
        assert!(require_tax_target_scope(&all, target_id).is_ok());

        for scope in [ScopeType::Self_, ScopeType::Team] {
            let filter = resolve_employee_scope_filter(
                &DatabaseConnection::Disconnected,
                tenant_id,
                scope,
                None,
            )
            .await
            .expect("missing viewer resolves to an empty target scope");
            assert!(matches!(filter, EmployeeScopeFilter::Empty));
            assert!(matches!(
                require_tax_target_scope(&filter, target_id),
                Err(KabiPayError::Forbidden(_))
            ));
        }
    }

    #[tokio::test]
    async fn every_protected_tax_query_uses_tax_read_before_db_access() {
        let employee_id = Uuid::new_v4();
        let fields = vec![
            "{ taxConfigurations { __typename } }".to_string(),
            "{ taxSectionDefinitions { __typename } }".to_string(),
            "{ taxSlabs { __typename } }".to_string(),
            format!("{{ taxComputations(employeeId: \"{employee_id}\") {{ __typename }} }}"),
            format!("{{ taxProofLines(employeeId: \"{employee_id}\") {{ __typename }} }}"),
        ];

        for query in fields {
            for denied_claims in [
                claims_without_permissions(),
                claims(PERM_TAX_APPROVE, Some("ALL")),
            ] {
                let response = execute_query(denied_claims, &query).await;
                assert_permission_denied_before_db(
                    &response,
                    &format!("{PERM_TAX_READ} permission required"),
                );
            }

            for scope in [None, Some("INVALID")] {
                let response = execute_query(claims(PERM_TAX_READ, scope), &query).await;
                assert_permission_denied_before_db(
                    &response,
                    &format!(
                        "{PERM_TAX_READ} permission requires an explicit valid scope"
                    ),
                );
            }
        }
    }
}
