use axum::{
    body::Bytes,
    http::{HeaderMap, StatusCode},
    Json,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::{
    db::{resolve_required_tenant_db, TenantDbCache, TenantDbConfig},
    KabiPayError, KabiPayResult,
};
use kabipay_organization_domain::{Capability, ResourceVersion};
use kabipay_organization_protocol::*;
use kabipay_organization_security::{
    self as security, AuthorityKind, CommandExpectation, DelegationClaims,
};
use sea_orm::{
    ConnectionTrait, DatabaseConnection, DatabaseTransaction, DbBackend, Statement,
    TransactionTrait, Value,
};
use std::{future::Future, time::Duration};
use uuid::Uuid;

pub trait SourceAdapter: Send + Sync {
    fn service(&self) -> &'static str;
    fn employee(
        &self,
        db: &DatabaseConnection,
        tenant: Uuid,
        command: &OrganizationCommand,
    ) -> impl Future<Output = KabiPayResult<Option<Uuid>>> + Send;
    fn prepare<'a>(
        &'a self,
        _tx: &'a DatabaseTransaction,
        _tenant: Uuid,
        _command: &'a OrganizationCommand,
    ) -> impl Future<Output = KabiPayResult<()>> + Send {
        async { Ok(()) }
    }
    fn execute(
        &self,
        db: &DatabaseTransaction,
        ops: &DatabaseConnection,
        authorization: &VerifiedRuntimeAuthorization,
        command: &OrganizationCommand,
    ) -> impl Future<Output = KabiPayResult<serde_json::Value>> + Send;
}

/// Constructible only after signature, exact intent, fresh policy and locked
/// source installation checks succeed. Wire DTOs cannot manufacture this proof.
pub struct VerifiedRuntimeAuthorization {
    pub(crate) policy: RuntimeAuthorization,
    pub(crate) operation: OrganizationOperation,
    pub(crate) command_id: Uuid,
    pub(crate) entitlements: Option<kabipay_common::entitlements::Entitlements>,
    pub(crate) local_user_id: Option<Uuid>,
    pub(crate) request_body: Vec<u8>,
    pub(crate) request_hash: String,
}
impl std::ops::Deref for VerifiedRuntimeAuthorization {
    type Target = RuntimeAuthorization;
    fn deref(&self) -> &Self::Target {
        &self.policy
    }
}
pub struct VerifiedPayrollAuthority<'a> {
    authorization: &'a VerifiedRuntimeAuthorization,
    entitlements: &'a kabipay_common::entitlements::Entitlements,
}
impl VerifiedRuntimeAuthorization {
    pub fn request_body(&self) -> &[u8] {
        &self.request_body
    }
    pub fn request_hash(&self) -> &str {
        &self.request_hash
    }
    pub fn command_id(&self) -> Uuid {
        self.command_id
    }
    pub fn local_user_id(&self) -> Option<Uuid> {
        self.local_user_id
    }
    pub fn payroll_business_date(&self) -> KabiPayResult<chrono::NaiveDate> {
        let value = self.entitlements.as_ref().ok_or_else(unavailable)?;
        value.require_tenant(self.tenant_id)?;
        value.require("PAYROLL")?;
        Ok(value.business_date())
    }
    pub fn payroll_authority(&self) -> KabiPayResult<VerifiedPayrollAuthority<'_>> {
        if !matches!(
            self.operation,
            OrganizationOperation::PayrollDraft | OrganizationOperation::PayrollFinalize
        ) || self.capability != "payroll:manage"
            || self.employee_scope != "ALL"
            || self.authority_kind != AuthorityKind::OrganizationGrant
        {
            return Err(unavailable());
        }
        let entitlements = self.entitlements.as_ref().ok_or_else(unavailable)?;
        entitlements.require_tenant(self.tenant_id)?;
        entitlements.require("PAYROLL")?;
        Ok(VerifiedPayrollAuthority {
            authorization: self,
            entitlements,
        })
    }
}
impl VerifiedPayrollAuthority<'_> {
    pub fn local_user_id(&self) -> Option<Uuid> {
        self.authorization.local_user_id()
    }
    pub fn entitlements(&self) -> &kabipay_common::entitlements::Entitlements {
        self.entitlements
    }
    pub fn tenant_id(&self) -> Uuid {
        self.authorization.tenant_id
    }
    pub fn principal_id(&self) -> Uuid {
        self.authorization.principal_id
    }
    pub fn command_id(&self) -> Uuid {
        self.authorization.command_id
    }
}

pub fn statement(sql: &str, values: Vec<Value>) -> Statement {
    Statement::from_sql_and_values(DbBackend::Postgres, sql, values)
}
pub fn unavailable() -> KabiPayError {
    KabiPayError::Forbidden(
        "Organization authority is unavailable or changed; reload the workspace".into(),
    )
}

/// Preserve the configured workflow relationship for a verified foreign actor.
/// A general company grant does not turn its holder into a reporting manager.
pub async fn assert_workflow_actor(
    tx: &DatabaseTransaction,
    p: &RuntimeAuthorization,
    subject: Uuid,
    approver_type: Option<&str>,
    permission: Option<&str>,
) -> KabiPayResult<()> {
    if p.employee_id != Some(subject) {
        return Err(unavailable());
    }
    if tx.query_one(statement("SELECT 1 AS n FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE') AND ($3<>tenant_id OR user_id IS DISTINCT FROM $4) FOR SHARE",vec![p.tenant_id.into(),subject.into(),p.source_tenant_id.into(),p.source_user_id.into()])).await?.is_none(){return Err(unavailable());}
    if tx.query_one(statement("SELECT 1 AS n FROM management_assignment WHERE tenant_id=$1 AND subject_employee_id=$2 AND subject_principal_id=$3",vec![p.tenant_id.into(),subject.into(),p.principal_id.into()])).await?.is_some(){return Err(unavailable());}
    let manager=tx.query_one(statement("SELECT 1 AS n FROM management_assignment a JOIN assignment_capability c ON c.tenant_id=a.tenant_id AND c.assignment_id=a.id WHERE a.tenant_id=$1 AND a.subject_employee_id=$2 AND a.manager_principal_id=$3 AND a.organization_id=$4 AND a.relationship_type='PRIMARY' AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) AND c.capability_code=$5 FOR SHARE OF a",vec![p.tenant_id.into(),subject.into(),p.principal_id.into(),p.organization_id.into(),p.capability.clone().into()])).await?.is_some();
    let allowed_permission = permission.is_some_and(|v| v.eq_ignore_ascii_case(&p.capability));
    let allowed = match approver_type
        .unwrap_or("REPORTING_MANAGER")
        .trim()
        .to_ascii_uppercase()
        .as_str()
    {
        "REPORTING_MANAGER" | "MANAGER" | "LINE_MANAGER" => manager,
        "PERMISSION" => allowed_permission,
        "REPORTING_MANAGER_OR_PERMISSION" | "MANAGER_OR_PERMISSION" => {
            manager || allowed_permission
        }
        _ => false,
    };
    if !allowed {
        return Err(unavailable());
    }
    Ok(())
}

pub async fn fixed_database(
    cache: &TenantDbCache,
    ops: &DatabaseConnection,
    fallback: &TenantDbConfig,
) -> KabiPayResult<(Uuid, DatabaseConnection)> {
    let tenant = cache.bound_tenant_id().ok_or_else(unavailable)?;
    let db = resolve_required_tenant_db(tenant, ops, cache, fallback).await?;
    Ok((tenant, db))
}

/// Authenticate the exact delegated target before opening a shared source pool.
pub async fn command_database(
    service: &str, headers: &HeaderMap, body: &[u8], cache: &TenantDbCache,
    ops: &DatabaseConnection, fallback: &TenantDbConfig,
) -> KabiPayResult<(Uuid, DatabaseConnection)> {
    let tenant = cache.requested_tenant(headers)?;
    let command = crate::parse_command(body)?;
    let query = parse_receipt_query(&command)?;
    let operation = query.as_ref().map(|q| q.original_operation).unwrap_or(command.operation);
    if operation.service() != service { return Err(unavailable()); }
    let token = headers.get("authorization").and_then(|v| v.to_str().ok())
        .and_then(|s| s.strip_prefix("Bearer ")).ok_or(KabiPayError::Unauthorised)?;
    let capability = Capability::parse(operation.capability()).map_err(|_| unavailable())?;
    let version = ResourceVersion::try_from(command.expected_version as u64).map_err(|_| unavailable())?;
    security::delegation_verifier_from_env(tenant, service).map_err(|_| unavailable())?
        .verify(token, &CommandExpectation {
            capability: &capability,
            resource_type: if query.is_some() { "organization_command" } else { operation.resource_type() },
            resource_id: command.resource_id, expected_resource_version: version,
            command_id: command.command_id, request_hash: &security::command_body_hash(body),
        }).map_err(|_| KabiPayError::Unauthorised)?;
    let db = resolve_required_tenant_db(tenant, ops, cache, fallback).await?;
    Ok((tenant, db))
}

pub async fn handle<A: SourceAdapter>(
    adapter: A,
    tenant: Uuid,
    db: DatabaseConnection,
    ops: &DatabaseConnection,
    headers: HeaderMap,
    body: Bytes,
) -> Result<Json<OrganizationCommandOutcome>, (StatusCode, String)> {
    run(&adapter, tenant, &db, ops, &headers, &body)
        .await
        .map(Json)
        .map_err(|error| {
            let status = command_error_status(&error);
            // Domain error conversion deliberately excludes database/transport details.
            let message = match error {
                KabiPayError::Validation(s)
                | KabiPayError::Forbidden(s)
                | KabiPayError::Conflict(s) => s,
                _ => "Organization command unavailable".into(),
            };
            (status, message)
        })
}
fn command_error_status(error: &KabiPayError) -> StatusCode {
    match error {
        KabiPayError::Unauthorised => StatusCode::UNAUTHORIZED,
        KabiPayError::Forbidden(_)
        | KabiPayError::ModuleNotSubscribed(_)
        | KabiPayError::TenantSuspended(_) => StatusCode::FORBIDDEN,
        KabiPayError::Validation(_) | KabiPayError::Json(_) | KabiPayError::BusinessRule { .. } => {
            StatusCode::BAD_REQUEST
        }
        KabiPayError::NotFound { .. } => StatusCode::NOT_FOUND,
        KabiPayError::Conflict(_)
        | KabiPayError::ConflictRule { .. }
        | KabiPayError::SeatLimitReached { .. } => StatusCode::CONFLICT,
        _ => StatusCode::SERVICE_UNAVAILABLE,
    }
}

pub(crate) async fn policy_post<T: serde::Serialize, R: serde::de::DeserializeOwned>(
    tenant: Uuid,
    route: &str,
    value: &T,
) -> KabiPayResult<R> {
    let raw = serde_json::to_vec(value)?;
    let base = std::env::var("KABIPAY_ORGANIZATION_POLICY_URL").map_err(|_| {
        kabipay_common::KabiPayError::Internal("Organization policy transport unavailable".into())
    })?;
    let url = reqwest::Url::parse(&base).map_err(|_| unavailable())?;
    let loopback = url
        .host_str()
        .and_then(|h| h.trim_matches(['[', ']']).parse::<std::net::IpAddr>().ok())
        .is_some_and(|ip| ip.is_loopback());
    if url.scheme() != "https"
        && !(url.scheme() == "http" && loopback)
        && std::env::var("KABIPAY_ORGANIZATION_PRIVATE_HTTP").as_deref() != Ok("1")
    {
        return Err(unavailable());
    }
    if !url.username().is_empty()
        || url.password().is_some()
        || url.query().is_some()
        || url.fragment().is_some()
    {
        return Err(unavailable());
    }
    let issuer = security::service_issuer_from_env().map_err(|_| unavailable())?;
    let credential = issuer
        .issue("kabipay-organization", tenant, route, &raw)
        .map_err(|_| unavailable())?;
    let client = kabipay_common::organization_http::client(Duration::from_secs(12))
        .map_err(|_| unavailable())?;
    let response = client
        .post(format!("{}{route}", base.trim_end_matches('/')))
        .bearer_auth(credential)
        .header("content-type", "application/json")
        .header("x-organization-company-id", tenant.to_string())
        .body(raw)
        .send()
        .await
        .map_err(|_| KabiPayError::Internal("Organization policy transport unavailable".into()))?;
    if !response.status().is_success() {
        return Err(
            if matches!(
                response.status().as_u16(),
                400 | 401 | 403 | 404 | 409 | 422
            ) {
                unavailable()
            } else {
                KabiPayError::Internal("Organization policy transport unavailable".into())
            },
        );
    }
    if response.content_length().is_some_and(|n| n > 262144) {
        return Err(unavailable());
    }
    let mut response = response;
    let mut bytes = Vec::new();
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|_| KabiPayError::Internal("Organization policy transport unavailable".into()))?
    {
        if bytes.len() + chunk.len() > 262144 {
            return Err(unavailable());
        }
        bytes.extend_from_slice(&chunk);
    }
    serde_json::from_slice(&bytes).map_err(|_| unavailable())
}

pub(crate) fn matches_policy(
    c: &DelegationClaims,
    p: &RuntimeAuthorization,
    employee: Option<Uuid>,
) -> bool {
    Uuid::from(c.organization_id) == p.organization_id
        && Uuid::from(c.target_membership_id) == p.membership_id
        && Uuid::from(c.target_tenant_id) == p.tenant_id
        && Uuid::from(c.sub) == p.principal_id
        && Uuid::from(c.source_tenant_id) == p.source_tenant_id
        && c.source_user_id == p.source_user_id
        && u64::from(c.identity_version) == p.identity_version as u64
        && u64::from(c.source_link_version) == p.source_link_version as u64
        && u64::from(c.target_membership_version) == p.membership_version as u64
        && c.authority_kind == p.authority_kind
        && c.authority_ref == p.authority_id
        && u64::from(c.authority_version) == p.authority_version as u64
        && c.capability.as_str() == p.capability
        && p.employee_id == employee
        && p.actor.principal_id == p.principal_id
        && p.actor.identity_version == p.identity_version
}

pub(crate) async fn local_authority(
    tx: &DatabaseTransaction,
    p: &RuntimeAuthorization,
    employee: Option<Uuid>,
) -> KabiPayResult<()> {
    if let Some(employee) = employee {
        if !(p.authority_kind==AuthorityKind::OrganizationGrant && p.employee_scope=="ALL") && tx.query_one(statement("SELECT 1 AS n FROM employee WHERE tenant_id=$1 AND id=$2 AND NOT is_deleted AND status IN ('ACTIVE','PROBATION','ON_LEAVE')",vec![p.tenant_id.into(),employee.into()])).await?.is_none() {return Err(unavailable());}
    }
    if p.authority_kind == AuthorityKind::OrganizationGrant && p.employee_scope != "ALL" {
        let actor_employee = p.scope_employee_id.ok_or_else(unavailable)?;
        let subject = employee.ok_or_else(unavailable)?;
        let scope = match p.employee_scope.as_str() {
            "SELF" => "subject.id=actor.id",
            "DEPARTMENT" => {
                "actor.department_id IS NOT NULL AND subject.department_id=actor.department_id"
            }
            _ => return Err(unavailable()),
        };
        let sql = format!("SELECT 1 AS n FROM employee actor JOIN employee subject ON subject.tenant_id=actor.tenant_id WHERE actor.tenant_id=$1 AND actor.id=$2 AND subject.id=$3 AND NOT actor.is_deleted AND NOT subject.is_deleted AND actor.status IN ('ACTIVE','PROBATION','ON_LEAVE') AND subject.status IN ('ACTIVE','PROBATION','ON_LEAVE') AND ({scope}) FOR SHARE OF actor,subject");
        if tx
            .query_one(statement(
                &sql,
                vec![p.tenant_id.into(), actor_employee.into(), subject.into()],
            ))
            .await?
            .is_none()
        {
            return Err(unavailable());
        }
    }
    let row=match p.authority_kind {
        AuthorityKind::OrganizationGrant=>tx.query_one(statement("SELECT 1 AS n FROM organization_grant_installation WHERE tenant_id=$1 AND binding_id=$2 AND principal_id=$3 AND organization_id=$4 AND membership_id=$5 AND grant_version=$6 AND membership_version=$7 AND status='ACTIVE' AND capabilities @> $8::jsonb FOR SHARE",vec![p.tenant_id.into(),p.authority_id.into(),p.principal_id.into(),p.organization_id.into(),p.membership_id.into(),p.authority_version.into(),p.membership_version.into(),serde_json::json!([{"code":p.capability,"employee_scope":p.employee_scope}]).into()])).await?,
        AuthorityKind::ManagementAssignment=>{
            let subject=employee.ok_or_else(unavailable)?;
            tx.query_one(statement("SELECT 1 AS n FROM management_assignment a JOIN assignment_capability c ON c.tenant_id=a.tenant_id AND c.assignment_id=a.id WHERE a.tenant_id=$1 AND a.id=$2 AND a.manager_principal_id=$3 AND a.organization_id=$4 AND a.membership_id=$5 AND a.version=$6 AND a.subject_employee_id=$7 AND c.capability_code=$8 AND a.status='ACTIVE' AND a.effective_from<=clock_timestamp() AND (a.effective_to IS NULL OR a.effective_to>clock_timestamp()) FOR SHARE OF a",vec![p.tenant_id.into(),p.authority_id.into(),p.principal_id.into(),p.organization_id.into(),p.membership_id.into(),p.authority_version.into(),subject.into(),p.capability.clone().into()])).await?
        }
    };
    if row.is_none() {
        return Err(unavailable());
    }
    Ok(())
}

#[derive(serde::Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptQuery {
    original_command_id: Uuid,
    original_operation: OrganizationOperation,
    request_hash: String,
    #[serde(default)]
    original_resource_id: Option<Uuid>,
}
fn parse_receipt_query(command: &OrganizationCommand) -> KabiPayResult<Option<ReceiptQuery>> {
    if !matches!(
        command.operation,
        OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
    ) {
        return Ok(None);
    }
    let query: ReceiptQuery = serde_json::from_value(command.payload.clone())?;
    if query.original_command_id != command.resource_id
        || matches!(
            query.original_operation,
            OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
        )
        || (command.operation == OrganizationOperation::CancelCommand)
            != command.payload.get("original_resource_id").is_some()
        || (command.operation == OrganizationOperation::CancelCommand
            && query.original_resource_id.is_none_or(|id| id.is_nil()))
        || query.request_hash.len() != 64
        || !query.request_hash.bytes().all(|c| c.is_ascii_hexdigit())
    {
        return Err(unavailable());
    }
    Ok(Some(query))
}

pub async fn run<A: SourceAdapter>(
    adapter: &A,
    tenant: Uuid,
    db: &DatabaseConnection,
    ops: &DatabaseConnection,
    headers: &HeaderMap,
    body: &[u8],
) -> KabiPayResult<OrganizationCommandOutcome> {
    let command = crate::parse_command(body)?;
    let token = headers
        .get("authorization")
        .and_then(|h| h.to_str().ok())
        .and_then(|h| h.strip_prefix("Bearer "))
        .filter(|s| !s.is_empty())
        .ok_or(KabiPayError::Unauthorised)?;
    let hash = security::command_body_hash(body);
    let wrapper = matches!(
        command.operation,
        OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
    );
    let receipt_query = parse_receipt_query(&command)?;
    let operation = receipt_query
        .as_ref()
        .map(|r| r.original_operation)
        .unwrap_or(command.operation);
    if matches!(
        operation,
        OrganizationOperation::ReadReceipt | OrganizationOperation::CancelCommand
    ) {
        return Err(unavailable());
    }
    if operation.service() != adapter.service() {
        return Err(unavailable());
    }
    let capability = Capability::parse(operation.capability()).map_err(|_| unavailable())?;
    let version =
        ResourceVersion::try_from(command.expected_version as u64).map_err(|_| unavailable())?;
    let verifier = security::delegation_verifier_from_env(tenant, adapter.service())
        .map_err(|_| unavailable())?;
    let verified = verifier
        .verify(
            token,
            &CommandExpectation {
                capability: &capability,
                resource_type: if wrapper {
                    "organization_command"
                } else {
                    operation.resource_type()
                },
                resource_id: command.resource_id,
                expected_resource_version: version,
                command_id: command.command_id,
                request_hash: &hash,
            },
        )
        .map_err(|_| KabiPayError::Unauthorised)?;
    let employee = if let Some(query) = receipt_query.as_ref() {
        // A status read is bound to the original immutable command and actor, and is
        // separately live-authorized. It is never an unauthenticated UUID probe.
        if query.original_command_id != command.resource_id {
            return Err(unavailable());
        }
        if (command.operation == OrganizationOperation::CancelCommand)
            != command.payload.get("original_resource_id").is_some()
            || (command.operation == OrganizationOperation::CancelCommand
                && query.original_resource_id.is_none())
            || query.request_hash.len() != 64
            || !query.request_hash.bytes().all(|c| c.is_ascii_hexdigit())
        {
            return Err(unavailable());
        }
        let (resource_id, expected_version) = if let Some(resource) = query.original_resource_id {
            (resource, command.expected_version)
        } else {
            let row=db.query_one(statement("SELECT operation,request_hash,resource_id,expected_version FROM organization_command_receipt WHERE tenant_id=$1 AND command_id=$2",vec![tenant.into(),command.resource_id.into()])).await?.ok_or_else(unavailable)?;
            let op: OrganizationOperation =
                serde_json::from_value(serde_json::Value::String(row.try_get("", "operation")?))?;
            if query.request_hash != row.try_get::<String>("", "request_hash")?
                || op != query.original_operation
                || command.expected_version != row.try_get::<i64>("", "expected_version")?
            {
                return Err(unavailable());
            }
            (
                row.try_get("", "resource_id")?,
                row.try_get("", "expected_version")?,
            )
        };
        let original = OrganizationCommand {
            command_id: query.original_command_id,
            resource_id,
            expected_version,
            operation,
            payload: serde_json::json!({}),
        };
        adapter.employee(db, tenant, &original).await?
    } else {
        adapter.employee(db, tenant, &command).await?
    };
    let policy: RuntimeAuthorization = policy_post(
        tenant,
        "/internal/organization/policy",
        &RuntimePolicyRequest {
            delegation_token: token.into(),
            command_body_base64: STANDARD.encode(body),
            employee_id: employee,
        },
    )
    .await?;
    if !matches_policy(verified.claims(), &policy, employee) {
        return Err(unavailable());
    }
    let entitlements = if !matches!(
        operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
            | OrganizationOperation::Directory
    ) {
        let current = kabipay_common::entitlements::Entitlements::load(ops, tenant).await?;
        if let Some(module) = kabipay_common::entitlements::service_module(adapter.service())? {
            current.require(module)?;
        }
        Some(current)
    } else {
        None
    };
    let metadata = matches!(
        operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
            | OrganizationOperation::Directory
    );
    let tx = db.begin().await?;
    if operation == OrganizationOperation::InstallMembership {
        crate::membership_lifecycle::lock_installation(&tx, tenant, policy.membership_id).await?;
    } else {
        crate::membership_lifecycle::require_active(
            &tx,
            tenant,
            policy.membership_id,
            policy.organization_id,
            policy.membership_version,
        )
        .await?;
    }

    tx.execute(statement(
        "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
        vec![format!(
            "org-command:{tenant}:{}",
            if wrapper {
                command.resource_id
            } else {
                command.command_id
            }
        )
        .into()],
    ))
    .await?;
    if !metadata && !wrapper {
        if matches!(
            operation,
            OrganizationOperation::EmployeeLoginProvision
                | OrganizationOperation::AccountCreate
                | OrganizationOperation::AccountUpdate
                | OrganizationOperation::AccountAccess
                | OrganizationOperation::AccountRoles
                | OrganizationOperation::EmployeeUpdate
                | OrganizationOperation::EmployeeEmployment
                | OrganizationOperation::SeparationCreate
                | OrganizationOperation::SeparationReject
                | OrganizationOperation::SeparationCancelApproval
        ) {
            tx.execute(statement(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                vec![format!("company-account-admin:{tenant}").into()],
            ))
            .await?;
        }
        adapter.prepare(&tx, tenant, &command).await?;
    }
    if !matches!(
        operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
            | OrganizationOperation::Directory
    ) {
        local_authority(&tx, &policy, employee).await?;
    }
    let lookup = if wrapper {
        command.resource_id
    } else {
        command.command_id
    };
    if let Some(row)=tx.query_one(statement("SELECT principal_id,source_tenant_id,source_user_id,organization_id,membership_id,authority_id,authority_version,operation,resource_id,expected_version,request_hash,outcome FROM organization_command_receipt WHERE tenant_id=$1 AND command_id=$2",vec![tenant.into(),lookup.into()])).await? {
        if row.try_get::<Uuid>("","principal_id")?!=policy.principal_id || row.try_get::<Uuid>("","source_tenant_id")?!=policy.source_tenant_id || row.try_get::<Uuid>("","source_user_id")?!=policy.source_user_id || row.try_get::<Uuid>("","organization_id")?!=policy.organization_id || row.try_get::<Uuid>("","membership_id")?!=policy.membership_id || (row.try_get::<String>("","request_hash")?!=receipt_query.as_ref().map(|q|q.request_hash.as_str()).unwrap_or(&hash)){return Err(unavailable());}
        if row.try_get::<Uuid>("","authority_id")?!=policy.authority_id || row.try_get::<i64>("","authority_version")?!=policy.authority_version{return Err(unavailable());}
        let stored_op:OrganizationOperation=serde_json::from_value(serde_json::Value::String(row.try_get("","operation")?))?;
        let stored_resource:Uuid=row.try_get("","resource_id")?;
        let expected_resource=receipt_query.as_ref().and_then(|q|q.original_resource_id).unwrap_or(if wrapper {stored_resource}else{command.resource_id});
        if stored_op!=operation || row.try_get::<i64>("","expected_version")?!=command.expected_version || stored_resource!=expected_resource {return Err(unavailable());}
        let outcome:OrganizationCommandOutcome=serde_json::from_value(row.try_get::<serde_json::Value>("","outcome")?)?;
        let ack=replay_ack_command(operation,lookup,&outcome,&policy)?;
        tx.commit().await?;
        if let Some(original)=ack {acknowledge(tenant,&policy,&original).await?;}
        return Ok(outcome);
    }
    if command.operation == OrganizationOperation::ReadReceipt {
        return Err(unavailable());
    }
    if command.operation == OrganizationOperation::CancelCommand {
        let query = receipt_query.as_ref().ok_or_else(unavailable)?;
        let original = OrganizationCommand {
            command_id: query.original_command_id,
            resource_id: query.original_resource_id.ok_or_else(unavailable)?,
            operation,
            expected_version: command.expected_version,
            payload: serde_json::json!({}),
        };
        if metadata {
            crate::installation::install_actor(&tx, tenant, &policy.actor).await?;
        } else {
            crate::installation::require_actor(&tx, tenant, &policy.actor).await?;
        }
        let outcome = OrganizationCommandOutcome {
            command_id: original.command_id,
            resource_id: original.resource_id,
            status: "CANCELLED_NOT_EXECUTED".into(),
            resource_version: original.expected_version,
            result: serde_json::json!({"cancelled":true}),
        };
        persist_receipt(
            &tx,
            tenant,
            &original,
            &policy,
            &query.request_hash,
            &outcome,
        )
        .await?;
        tx.commit().await?;
        return Ok(outcome);
    }
    let source_resource_type = if operation == OrganizationOperation::FeedbackCreate {
        "employee"
    } else {
        operation.resource_type()
    };
    if command.operation.is_company_collection()
        && (command.resource_id != policy.membership_id
            || command.expected_version != policy.membership_version)
    {
        return Err(unavailable());
    }
    if !matches!(
        command.operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
            | OrganizationOperation::Directory
    ) && !command.operation.is_company_collection()
    {
        // Domain rows precede version rows, matching existing local mutations and
        // their AFTER triggers; reversing these locks deadlocks local decisions.
        let table = match source_resource_type {
            "employee" => "employee",
            "leave_request" => "leave_request",
            "timesheet" => "timesheet_week_batch",
            "performance_participant" => "performance_participant",
            "payroll_run" => "payroll_cycle",
            "payroll_job" => "organization_payroll_job",
            "user" => "\"user\"",
            "workflow_definition" => "workflow",
            "separation" => "separation",
            "clearance_checklist" => "clearance_checklist",
            _ => return Err(unavailable()),
        };
        if command.operation.may_create_resource() {
            tx.execute(statement(
                "SELECT pg_advisory_xact_lock(hashtextextended($1,0))",
                vec![format!(
                    "org-create:{tenant}:{source_resource_type}:{}",
                    command.resource_id
                )
                .into()],
            ))
            .await?;
        }
        let exists = tx
            .query_one(statement(
                &format!("SELECT id FROM {table} WHERE tenant_id=$1 AND id=$2 FOR UPDATE"),
                vec![tenant.into(), command.resource_id.into()],
            ))
            .await?
            .is_some();
        if !exists {
            if !command.operation.may_create_resource() || command.expected_version != 1 {
                return Err(unavailable());
            }
            tx.execute(statement("INSERT INTO organization_resource_version(tenant_id,resource_type,resource_id,version) VALUES($1,$2,$3,1) ON CONFLICT DO NOTHING",vec![tenant.into(),source_resource_type.into(),command.resource_id.into()])).await?;
        }
        let version=tx.query_one(statement("SELECT version FROM organization_resource_version WHERE tenant_id=$1 AND resource_type=$2 AND resource_id=$3 FOR UPDATE",vec![tenant.into(),source_resource_type.into(),command.resource_id.into()])).await?.ok_or_else(unavailable)?.try_get::<i64>("","version")?;
        if version != command.expected_version {
            return Err(KabiPayError::Conflict(
                "Source record changed; reload before continuing".into(),
            ));
        }
    }
    if !matches!(
        operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
            | OrganizationOperation::Directory
    ) {
        local_authority(&tx, &policy, employee).await?;
    }
    let local_user_id = if policy.source_tenant_id == tenant {
        if tx.query_one(statement("SELECT id FROM \"user\" WHERE tenant_id=$1 AND id=$2 AND is_active AND NOT is_deleted FOR SHARE",vec![tenant.into(),policy.source_user_id.into()])).await?.is_none() {return Err(unavailable());}
        Some(policy.source_user_id)
    } else {
        None
    };
    if metadata {
        let mut actors = vec![policy.actor.clone()];
        if let Some(g) = &policy.grant {
            actors.push(g.actor.clone());
        }
        if let Some(r) = &policy.relationship {
            actors.extend(r.actors.clone());
        }
        actors.sort_by_key(|a| a.principal_id);
        for pair in actors.windows(2) {
            if pair[0].principal_id == pair[1].principal_id
                && serde_json::to_value(&pair[0])? != serde_json::to_value(&pair[1])?
            {
                return Err(unavailable());
            }
        }
        actors.dedup_by_key(|a| a.principal_id);
        for actor in actors {
            crate::installation::install_actor(&tx, tenant, &actor).await?;
        }
    } else {
        crate::installation::require_actor(&tx, tenant, &policy.actor).await?;
    }

    let event_actor = CompanyEventActor::OrganizationPrincipal {
        principal_id: policy.principal_id,
        display_name: policy.actor.display_name.clone(),
        identity_version: policy.identity_version,
        source_tenant_id: policy.source_tenant_id,
        source_user_id: policy.source_user_id,
        authority_kind: policy.authority_kind.clone(),
        authority_id: policy.authority_id,
        authority_version: policy.authority_version,
        command_id: command.command_id,
    };
    tx.execute(statement(
        "SELECT set_config('kabipay.organization_actor',$1,true)",
        vec![serde_json::to_string(&event_actor)?.into()],
    ))
    .await?;
    let result = match command.operation {
        OrganizationOperation::InstallMembership => {
            let _: crate::EmptyPayload = serde_json::from_value(command.payload.clone())?;
            let membership = policy.membership.as_ref().ok_or_else(unavailable)?;
            if membership.tenant_id != tenant
                || membership.organization_id != policy.organization_id
                || membership.membership_id != policy.membership_id
                || membership.membership_id != command.resource_id
                || membership.version != command.expected_version
                || membership.version != policy.membership_version
            {
                return Err(unavailable());
            }
            serde_json::to_value(
                crate::membership_lifecycle::install(&tx, membership, command.command_id).await?,
            )?
        }
        OrganizationOperation::InstallGrant => {
            crate::installation::install_grant(&tx, &policy, &command).await?
        }
        OrganizationOperation::InstallRelationship => {
            crate::installation::install_relationship(&tx, &policy, &command).await?
        }
        _ => {
            adapter
                .execute(
                    &tx,
                    ops,
                    &VerifiedRuntimeAuthorization {
                        policy: policy.clone(),
                        operation: command.operation,
                        command_id: command.command_id,
                        entitlements,
                        local_user_id,
                        request_body: body.to_vec(),
                        request_hash: hash.clone(),
                    },
                    &command,
                )
                .await?
        }
    };
    let resource_version=tx.query_one(statement("SELECT version FROM organization_resource_version WHERE tenant_id=$1 AND resource_type=$2 AND resource_id=$3",vec![tenant.into(),source_resource_type.into(),command.resource_id.into()])).await?.map(|r|r.try_get::<i64>("","version")).transpose()?.unwrap_or(command.expected_version);
    let outcome = OrganizationCommandOutcome {
        command_id: command.command_id,
        resource_id: command.resource_id,
        status: if matches!(
            command.operation,
            OrganizationOperation::PayrollDraft | OrganizationOperation::PayrollFinalize
        ) {
            "ACCEPTED"
        } else {
            "COMMITTED"
        }
        .into(),
        resource_version,
        result,
    };
    if command.operation.is_read() {
        if !metadata {
            crate::installation::refresh_actor(&tx, tenant, &policy.actor).await?;
        }
        tx.commit().await?;
        return Ok(outcome);
    }
    persist_receipt(&tx, tenant, &command, &policy, &hash, &outcome).await?;
    if command.operation.is_company_administration() {
        let action = serde_json::to_value(command.operation)?
            .as_str()
            .ok_or_else(unavailable)?
            .to_owned();
        tx.execute(statement(
            "SELECT organization_emit_administration($1,$2,$3,$4,$5,$6,'RECORDED')",
            vec![
                tenant.into(),
                policy.organization_id.into(),
                source_resource_type.into(),
                command.resource_id.into(),
                resource_version.into(),
                action.into(),
            ],
        ))
        .await?;
    }
    if !command.operation.is_read() {
        tx.execute(statement("INSERT INTO organization_business_actor(tenant_id,resource_type,resource_id,principal_id,command_id,action) VALUES($1,$2,$3,$4,$5,$6)",vec![tenant.into(),operation.resource_type().into(),command.resource_id.into(),policy.principal_id.into(),command.command_id.into(),policy.capability.clone().into()])).await?;
    }
    if !metadata {
        crate::installation::refresh_actor(&tx, tenant, &policy.actor).await?;
    }
    tx.commit().await?;
    if matches!(
        command.operation,
        OrganizationOperation::InstallMembership
            | OrganizationOperation::InstallGrant
            | OrganizationOperation::InstallRelationship
    ) {
        acknowledge(tenant, &policy, &command).await?;
    }
    Ok(outcome)
}

async fn persist_receipt(
    tx: &DatabaseTransaction,
    tenant: Uuid,
    command: &OrganizationCommand,
    policy: &RuntimeAuthorization,
    hash: &str,
    outcome: &OrganizationCommandOutcome,
) -> KabiPayResult<()> {
    let op = serde_json::to_value(command.operation)?
        .as_str()
        .ok_or_else(unavailable)?
        .to_owned();
    tx.execute(statement("INSERT INTO organization_command_receipt(tenant_id,command_id,principal_id,source_tenant_id,source_user_id,organization_id,membership_id,authority_id,authority_version,resource_id,operation,request_hash,expected_version,outcome) VALUES($1,$2,$3,$4,$5,$6,$7,$8,$9,$10,$11,$12,$13,$14)",vec![tenant.into(),command.command_id.into(),policy.principal_id.into(),policy.source_tenant_id.into(),policy.source_user_id.into(),policy.organization_id.into(),policy.membership_id.into(),policy.authority_id.into(),policy.authority_version.into(),command.resource_id.into(),op.into(),hash.to_owned().into(),command.expected_version.into(),serde_json::to_value(outcome)?.into()])).await?;
    Ok(())
}

/// An installation receipt can attest only the exact source-confirmed
/// configuration, including when a new status command recovers a lost ACK.
fn replay_ack_command(
    operation: OrganizationOperation,
    original: Uuid,
    outcome: &OrganizationCommandOutcome,
    p: &RuntimeAuthorization,
) -> KabiPayResult<Option<OrganizationCommand>> {
    if outcome.command_id != original {
        return Err(unavailable());
    }
    if outcome.status == "CANCELLED_NOT_EXECUTED" {
        return Ok(None);
    }
    let (resource, version, hash) = match operation {
        OrganizationOperation::InstallMembership => {
            let m = p.membership.as_ref().ok_or_else(unavailable)?;
            (m.membership_id, m.version, &m.configuration_hash)
        }
        OrganizationOperation::InstallGrant => {
            let g = p.grant.as_ref().ok_or_else(unavailable)?;
            (g.binding_id, g.grant_version, &g.configuration_hash)
        }
        OrganizationOperation::InstallRelationship => {
            let r = p.relationship.as_ref().ok_or_else(unavailable)?;
            (r.id, r.version, &r.configuration_hash)
        }
        _ => return Ok(None),
    };
    if outcome.command_id != original
        || outcome.resource_id != resource
        || outcome.status != "COMMITTED"
        || outcome.resource_version != version
        || outcome
            .result
            .get("configuration_hash")
            .and_then(serde_json::Value::as_str)
            != Some(hash.as_str())
    {
        return Err(unavailable());
    }
    Ok(Some(OrganizationCommand {
        command_id: original,
        resource_id: resource,
        expected_version: version,
        operation,
        payload: serde_json::json!({}),
    }))
}

async fn acknowledge(
    tenant: Uuid,
    p: &RuntimeAuthorization,
    c: &OrganizationCommand,
) -> KabiPayResult<()> {
    let _: serde_json::Value = match c.operation {
        OrganizationOperation::InstallMembership => {
            let m = p.membership.as_ref().ok_or_else(unavailable)?;
            policy_post(
                tenant,
                "/internal/organization/memberships/ack",
                &MembershipAcknowledgement {
                    membership_id: m.membership_id,
                    status: if m.status == "DETACHING" {
                        "DETACHED".into()
                    } else {
                        m.status.clone()
                    },
                    version: m.version,
                    configuration_hash: m.configuration_hash.clone(),
                    command_id: c.command_id,
                },
            )
            .await?
        }
        OrganizationOperation::InstallGrant => {
            let g = p.grant.as_ref().ok_or_else(unavailable)?;
            policy_post(
                tenant,
                "/internal/organization/grants/ack",
                &GrantAcknowledgement {
                    membership_id: g.membership_id,
                    binding_id: g.binding_id,
                    grant_version: g.grant_version,
                    configuration_hash: g.configuration_hash.clone(),
                    command_id: c.command_id,
                },
            )
            .await?
        }
        OrganizationOperation::InstallRelationship => {
            let r = p.relationship.as_ref().ok_or_else(unavailable)?;
            policy_post(
                tenant,
                "/internal/organization/relationships/ack",
                &RelationshipAcknowledgement {
                    membership_id: r.membership_id,
                    assignment_id: r.id,
                    version: r.version,
                    graph_version: r.graph_version,
                    configuration_hash: r.configuration_hash.clone(),
                    command_id: c.command_id,
                },
            )
            .await?
        }
        _ => return Ok(()),
    };
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn definitive_source_rules_are_rejected_while_outages_remain_unknown() {
        assert_eq!(
            command_error_status(&KabiPayError::BusinessRule {
                code: "REQUIRED",
                message: "required".into()
            }),
            StatusCode::BAD_REQUEST
        );
        assert_eq!(
            command_error_status(&KabiPayError::ModuleNotSubscribed("PAYROLL".into())),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            command_error_status(&KabiPayError::SeatLimitReached {
                module_code: "EMPLOYEE".into(),
                contracted: 1,
                current: 1
            }),
            StatusCode::CONFLICT
        );
        assert_eq!(
            command_error_status(&KabiPayError::Internal("outage".into())),
            StatusCode::SERVICE_UNAVAILABLE
        );
    }
    fn fixture() -> (DelegationClaims, RuntimeAuthorization, Uuid) {
        let org = Uuid::new_v4();
        let membership = Uuid::new_v4();
        let tenant = Uuid::new_v4();
        let source = Uuid::new_v4();
        let user = Uuid::new_v4();
        let principal = Uuid::new_v4();
        let authority = Uuid::new_v4();
        let employee = Uuid::new_v4();
        let version = ResourceVersion::try_from(1).unwrap();
        let claims = DelegationClaims {
            iss: security::DELEGATION_ISSUER.into(),
            aud: "kabipay-leave".into(),
            sub: principal.try_into().unwrap(),
            jti: Uuid::new_v4(),
            iat: 1,
            nbf: 1,
            exp: 2,
            organization_id: org.try_into().unwrap(),
            target_membership_id: membership.try_into().unwrap(),
            source_tenant_id: source.try_into().unwrap(),
            source_user_id: user,
            target_tenant_id: tenant.try_into().unwrap(),
            authority_kind: AuthorityKind::OrganizationGrant,
            authority_ref: authority,
            authority_version: version,
            identity_version: version,
            source_link_version: version,
            target_membership_version: version,
            capability: Capability::parse("leave:approve").unwrap(),
            resource_type: "leave_request".into(),
            resource_id: Uuid::new_v4(),
            expected_resource_version: version,
            command_id: Uuid::new_v4(),
            request_hash: "a".repeat(64),
        };
        let policy = RuntimeAuthorization {
            organization_id: org,
            membership_id: membership,
            tenant_id: tenant,
            principal_id: principal,
            source_tenant_id: source,
            source_user_id: user,
            identity_version: 1,
            source_link_version: 1,
            membership_version: 1,
            authority_kind: AuthorityKind::OrganizationGrant,
            authority_id: authority,
            authority_version: 1,
            capability: "leave:approve".into(),
            employee_scope: "ALL".into(),
            employee_id: Some(employee),
            scope_employee_id: None,
            actor_target_user_ids: Vec::new(),
            actor: ActorReference {
                principal_id: principal,
                display_name: "Remote manager".into(),
                identity_version: 1,
            },
            grant: None,
            relationship: None,
            membership: None,
        };
        (claims, policy, employee)
    }
    #[test]
    fn live_policy_must_match_all_identity_and_membership_epochs() {
        let (claims, policy, employee) = fixture();
        assert!(matches_policy(&claims, &policy, Some(employee)));
        for field in 0..3 {
            let mut changed = policy.clone();
            match field {
                0 => changed.identity_version = 2,
                1 => changed.source_link_version = 2,
                _ => changed.membership_version = 2,
            };
            assert!(!matches_policy(&claims, &changed, Some(employee)));
        }
    }
    #[test]
    fn cannot_substitute_company_actor_or_subject_from_live_policy() {
        let (claims, policy, employee) = fixture();
        let mut changed = policy.clone();
        changed.tenant_id = Uuid::new_v4();
        assert!(!matches_policy(&claims, &changed, Some(employee)));
        let mut changed = policy.clone();
        changed.actor.principal_id = Uuid::new_v4();
        assert!(!matches_policy(&claims, &changed, Some(employee)));
        assert!(!matches_policy(&claims, &policy, Some(Uuid::new_v4())));
    }
    #[test]
    fn payroll_proof_is_restricted_to_explicit_companywide_mutations() {
        let (_, mut policy, _) = fixture();
        let command_id = Uuid::new_v4();
        policy.capability = "payroll:manage".into();
        let mut verified = VerifiedRuntimeAuthorization {
            policy,
            operation: OrganizationOperation::PayrollDraft,
            command_id,
            entitlements: None,
            local_user_id: None,
            request_body: Vec::new(),
            request_hash: String::new(),
        };
        assert!(
            verified.payroll_authority().is_err(),
            "wire authority alone cannot supply module entitlements"
        );
        verified.operation = OrganizationOperation::LeaveDecision;
        assert!(verified.payroll_authority().is_err());
        verified.operation = OrganizationOperation::PayrollFinalize;
        verified.policy.employee_scope = "TEAM".into();
        assert!(verified.payroll_authority().is_err());
        verified.policy.employee_scope = "ALL".into();
        verified.policy.authority_kind = AuthorityKind::ManagementAssignment;
        assert!(verified.payroll_authority().is_err());
    }
    #[test]
    fn recovery_wrappers_require_exact_non_nested_original_intent() {
        let id = Uuid::new_v4();
        let mut command = OrganizationCommand {
            command_id: Uuid::new_v4(),
            resource_id: id,
            expected_version: 1,
            operation: OrganizationOperation::ReadReceipt,
            payload: serde_json::json!({"original_command_id":id,"original_operation":"leave_decision","request_hash":"a".repeat(64)}),
        };
        assert!(parse_receipt_query(&command).is_ok());
        command.payload["original_resource_id"] = serde_json::json!(Uuid::new_v4());
        assert!(parse_receipt_query(&command).is_err());
        command.operation = OrganizationOperation::CancelCommand;
        assert!(parse_receipt_query(&command).is_ok());
        command.payload["original_operation"] = serde_json::json!("cancel_command");
        assert!(parse_receipt_query(&command).is_err());
        command.payload["original_operation"] = serde_json::json!("leave_decision");
        command.payload["original_resource_id"] = serde_json::Value::Null;
        assert!(parse_receipt_query(&command).is_err());
    }
    #[test]
    fn status_recovery_ack_uses_original_intent_and_exact_confirmed_hash() {
        let (_, mut policy, _) = fixture();
        let original = Uuid::new_v4();
        let hash = "a".repeat(64);
        policy.membership = Some(MembershipInstallation {
            organization_id: policy.organization_id,
            membership_id: policy.membership_id,
            tenant_id: policy.tenant_id,
            status: "DETACHING".into(),
            version: 2,
            configuration_hash: hash.clone(),
        });
        let mut outcome = OrganizationCommandOutcome {
            command_id: original,
            resource_id: policy.membership_id,
            status: "COMMITTED".into(),
            resource_version: 2,
            result: serde_json::json!({"configuration_hash":hash,"status":"DETACHED"}),
        };
        let ack = replay_ack_command(
            OrganizationOperation::InstallMembership,
            original,
            &outcome,
            &policy,
        )
        .unwrap()
        .unwrap();
        assert_eq!(ack.command_id, original);
        assert_eq!(ack.resource_id, policy.membership_id);
        assert_eq!(ack.operation, OrganizationOperation::InstallMembership);
        assert_eq!(ack.expected_version, 2);
        outcome.result["configuration_hash"] = serde_json::json!("b".repeat(64));
        assert!(replay_ack_command(
            OrganizationOperation::InstallMembership,
            original,
            &outcome,
            &policy
        )
        .is_err());
        outcome.status = "CANCELLED_NOT_EXECUTED".into();
        assert!(replay_ack_command(
            OrganizationOperation::InstallMembership,
            original,
            &outcome,
            &policy
        )
        .unwrap()
        .is_none());
        assert!(replay_ack_command(
            OrganizationOperation::InstallMembership,
            Uuid::new_v4(),
            &outcome,
            &policy
        )
        .is_err());
        outcome.status = "COMMITTED".into();
        outcome.result["configuration_hash"] = serde_json::json!(hash);
        assert!(replay_ack_command(
            OrganizationOperation::InstallMembership,
            Uuid::new_v4(),
            &outcome,
            &policy
        )
        .is_err());
    }
}
