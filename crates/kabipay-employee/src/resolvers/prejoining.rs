//! Dedicated tenant-wide pre-joining authorization and GraphQL DTOs.
use async_graphql::{Context, ID, InputObject, Json, SimpleObject};
use chrono::{DateTime, NaiveDate, Utc};
use sea_orm::{
    ColumnTrait, EntityTrait, PaginatorTrait, QueryFilter, QueryOrder,
    QuerySelect,
};
use serde_json::{json, Value};
use uuid::Uuid;
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::{
    KabiPayError,
    subgraph::{require_client_claims, require_tenant_id, tenant_db},
};
use kabipay_db_entities::tenant::d0080_prejoining::prejoining_candidate as candidate;
use crate::services::prejoining as service;
use super::scope::require_exact_all_scope;
type Result<T> = async_graphql::Result<T>;
#[derive(SimpleObject)]
pub struct PrejoiningCandidate {
    pub id: ID,
    pub email: String,
    pub status: String,
    pub revision: i32,
    pub config: Json<Value>,
    pub answers: Json<Value>,
    pub feedback: Option<String>,
    pub documents: Json<Value>,
    pub expires_at: Option<DateTime<Utc>>,
    pub employee_id: Option<ID>,
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
#[derive(SimpleObject)]
pub struct PrejoiningPage {
    pub nodes: Vec<PrejoiningCandidate>,
    pub total: i32,
}
#[derive(SimpleObject)]
pub struct PrejoiningInvitation {
    pub candidate: PrejoiningCandidate,
    pub private_url: String,
    pub email_status: String,
    pub email_error: Option<String>,
}
#[derive(SimpleObject)]
pub struct PrejoiningDocumentContent {
    pub filename: String,
    pub mime_type: String,
    pub base64_content: String,
}
#[derive(InputObject)]
pub struct ConfirmPrejoiningInput {
    pub candidate_id: ID,
    pub revision: i32,
    pub employee_code: String,
    pub date_of_joining: NaiveDate,
    pub department_id: Option<ID>,
    pub designation_id: Option<ID>,
    pub reporting_manager_id: Option<ID>,
    pub employment_type: Option<String>,
    pub username: String,
    pub initial_password: String,
    pub role_ids: Vec<ID>,
}
pub fn gate(ctx: &Context<'_>, permission: &str) -> Result<()> {
    require_exact_all_scope(ctx, permission)
}
pub fn config_gate(ctx: &Context<'_>) -> Result<()> {
    gate(ctx, "prejoining:manage").or_else(|_| gate(ctx, "prejoining:review"))
}
pub fn id(value: &ID) -> Result<Uuid> {
    Uuid::parse_str(value.as_str()).map_err(|_|
            service::validation("invalid ID").into_graphql())
}
pub async fn dto(db: &sea_orm::DatabaseConnection, row: candidate::Model)
    -> Result<PrejoiningCandidate> {
    let docs =
        service::documents(db,
                        &row).await.map_err(KabiPayError::into_graphql)?;
    Ok(PrejoiningCandidate {
            id: row.id.into(),
            email: row.email,
            status: row.status,
            revision: row.revision,
            config: Json(row.config),
            answers: Json(row.answers),
            feedback: row.feedback,
            documents: Json(json!(docs.iter().map(service::document_metadata).collect::<Vec<_>>())),
            expires_at: row.expires_at,
            employee_id: row.employee_id.map(Into::into),
            created_at: row.created_at,
            updated_at: row.updated_at,
        })
}
pub async fn config(ctx: &Context<'_>) -> Result<Json<Value>> {
    config_gate(ctx)?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    Ok(Json(service::get_config(&db,
                            tenant).await.map_err(KabiPayError::into_graphql)?))
}
pub fn catalog(ctx: &Context<'_>) -> Result<Json<Value>> {
    config_gate(ctx)?;
    Ok(Json(json!(service::FIELD_KEYS.iter().map(|key|json!({
                    "key":key, "label":service::field_label(key),
                    "required":matches!(*key,"firstName"|"lastName"|"email")
                })).collect::<Vec<_>>())))
}
pub async fn document_types(ctx: &Context<'_>) -> Result<Json<Value>> {
    config_gate(ctx)?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    use kabipay_db_entities::tenant::d0008_document_system::document_type;
    let rows = document_type::Entity::find()
        .filter(document_type::Column::TenantId.eq(tenant))
        .filter(document_type::Column::IsDeleted.eq(false))
        .order_by_asc(document_type::Column::Name)
        .order_by_asc(document_type::Column::Id)
        .all(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    Ok(Json(json!(rows.into_iter().map(|r|json!({
                    "id":r.id, "name":r.name
                })).collect::<Vec<_>>())))
}
pub async fn list(ctx: &Context<'_>, offset: Option<i32>, limit: Option<i32>,
    status: Option<String>) -> Result<PrejoiningPage> {
    gate(ctx, "prejoining:review")?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    let mut query =
        candidate::Entity::find().filter(candidate::Column::TenantId.eq(tenant));
    if let Some(status) = status.filter(|s| !s.is_empty()) {
        query = query.filter(candidate::Column::Status.eq(status));
    }
    let total =
        query.clone().count(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    let rows =
        query.order_by_desc(candidate::Column::CreatedAt).order_by_desc(candidate::Column::Id).offset(offset.unwrap_or(0).max(0)
                                        as
                                        u64).limit(limit.unwrap_or(25).clamp(1, 100) as
                                    u64).all(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    let mut nodes = Vec::new();
    for row in rows { nodes.push(dto(&db, row).await?); }
    Ok(PrejoiningPage {
            nodes,
            total: i32::try_from(total).unwrap_or(i32::MAX),
        })
}
pub async fn detail(ctx: &Context<'_>, value: ID)
    -> Result<Option<PrejoiningCandidate>> {
    gate(ctx, "prejoining:review")?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    match service::find(&db, tenant,
                        id(&value)?).await.map_err(KabiPayError::into_graphql)? {
        Some(row) => Ok(Some(dto(&db, row).await?)),
        None => Ok(None),
    }
}
pub async fn download(ctx: &Context<'_>, candidate_id: ID, document_id: ID)
    -> Result<PrejoiningDocumentContent> {
    gate(ctx, "prejoining:review")?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    let row =
        service::find(&db, tenant,
                                id(&candidate_id)?).await.map_err(KabiPayError::into_graphql)?.ok_or_else(||
                    service::validation("candidate unavailable").into_graphql())?;
    let doc =
        service::get_document(&db, &row,
                        id(&document_id)?).await.map_err(KabiPayError::into_graphql)?;
    Ok(PrejoiningDocumentContent {
            filename: doc.filename,
            mime_type: doc.mime_type,
            base64_content: STANDARD.encode(doc.bytes),
        })
}
pub async fn save_config(ctx: &Context<'_>, config: Json<Value>)
    -> Result<Json<Value>> {
    gate(ctx, "prejoining:manage")?;
    let tenant = require_tenant_id(ctx)?;
    let actor = require_client_claims(ctx)?.sub;
    let db = tenant_db(ctx, tenant).await?;
    Ok(Json(service::save_config(&db, tenant, actor,
                            config.0).await.map_err(KabiPayError::into_graphql)?))
}
async fn invitation(db: &sea_orm::DatabaseConnection, row: candidate::Model,
    url: String, send_email: bool) -> Result<PrejoiningInvitation> {
    let (status, error) =
        if send_email {
            match service::send_email(&row.email, &url).await {
                Ok(()) => ("SENT", None),
                Err(_) =>
                    ("FAILED",
                        Some("Email delivery failed or is not configured. Copy the private link.".into())),
            }
        } else { ("NOT_REQUESTED", None) };
    Ok(PrejoiningInvitation {
            candidate: dto(db, row).await?,
            private_url: url,
            email_status: status.into(),
            email_error: error,
        })
}
pub async fn invite(ctx: &Context<'_>, email: String, send_email: bool)
    -> Result<PrejoiningInvitation> {
    gate(ctx, "prejoining:manage")?;
    let tenant = require_tenant_id(ctx)?;
    let actor = require_client_claims(ctx)?.sub;
    let db = tenant_db(ctx, tenant).await?;
    let (row, url) =
        service::invite(&db, tenant, actor,
                        &email).await.map_err(KabiPayError::into_graphql)?;
    invitation(&db, row, url, send_email).await
}
pub async fn reissue(ctx: &Context<'_>, value: ID, revision: i32,
    send_email: bool) -> Result<PrejoiningInvitation> {
    gate(ctx, "prejoining:manage")?;
    gate(ctx, "prejoining:review")?;
    let tenant = require_tenant_id(ctx)?;
    let actor = require_client_claims(ctx)?.sub;
    let db = tenant_db(ctx, tenant).await?;
    let (row, url) =
        service::reissue(&db, tenant, id(&value)?, revision,
                        actor).await.map_err(KabiPayError::into_graphql)?;
    invitation(&db, row, url, send_email).await
}
pub async fn review(ctx: &Context<'_>, value: ID, revision: i32, action: &str,
    feedback: Option<String>) -> Result<PrejoiningCandidate> {
    gate(ctx, "prejoining:review")?;
    gate(ctx,
            if action == "CANCEL" {
                "prejoining:manage"
            } else { "prejoining:review" })?;
    let tenant = require_tenant_id(ctx)?;
    let actor = require_client_claims(ctx)?.sub;
    let db = tenant_db(ctx, tenant).await?;
    let row =
        service::review(&db, tenant, id(&value)?, revision, actor, action,
                        feedback).await.map_err(KabiPayError::into_graphql)?;
    dto(&db, row).await
}
pub async fn confirm(ctx: &Context<'_>, input: ConfirmPrejoiningInput)
    -> Result<PrejoiningCandidate> {
    gate(ctx, "prejoining:review")?;
    super::scope::require_tenant_rbac_admin(ctx)?;
    super::mutation::require_employee_mutation_rbac(ctx)?;
    gate(ctx, "employee:write").or_else(|_| gate(ctx, "employee:manage"))?;
    let tenant = require_tenant_id(ctx)?;
    let actor = require_client_claims(ctx)?.sub;
    let candidate_id = id(&input.candidate_id)?;
    let db = tenant_db(ctx, tenant).await?;
    if let Some(row) = service::find(&db, tenant, candidate_id).await.map_err(KabiPayError::into_graphql)? {
        if row.status == "JOINED" {
            return dto(&db, row).await;
        }
    }
    let password =
        super::mutation::validate_admin_password(input.initial_password,
                "initialPassword")?;
    let password_hash = super::mutation::hash_password_async(password).await?;
    let optional_id = |value: Option<ID>| value.map(|v| id(&v)).transpose();
    let data =
        crate::services::employee_service::NewEmployee {
            employee_code: input.employee_code.trim().into(),
            first_name: String::new(),
            last_name: String::new(),
            date_of_joining: input.date_of_joining,
            department_id: optional_id(input.department_id)?,
            designation_id: optional_id(input.designation_id)?,
            reporting_manager_id: optional_id(input.reporting_manager_id)?,
            employment_type: input.employment_type,
            status: "ACTIVE".into(),
            user_id: None,
        };
    let account =
        crate::services::employee_service::NewLoginAccount {
            username: input.username,
            email: None,
            password_hash,
            role_ids: input.role_ids.iter().map(id).collect::<Result<Vec<_>>>()?,
        };
    let row =
        service::confirm(&db, tenant, candidate_id, input.revision, actor,
                        data, account).await.map_err(KabiPayError::into_graphql)?;
    dto(&db, row).await
}
fn csv_cell(value: &str) -> String {
    let prefix =
        if value.trim_start_matches(|ch: char| ch.is_whitespace() || ch.is_control() || ch == '\u{feff}').starts_with(['=', '+', '-', '@']) {
            "'"
        } else { "" };
    format!("\"{prefix}{}\"",value.replace('"',"\"\""))
}
pub async fn csv(ctx: &Context<'_>, status: Option<String>)
    -> Result<String> {
    gate(ctx, "prejoining:review")?;
    let tenant = require_tenant_id(ctx)?;
    let db = tenant_db(ctx, tenant).await?;
    let mut query =
        candidate::Entity::find().filter(candidate::Column::TenantId.eq(tenant));
    if let Some(status) = status.filter(|s| !s.is_empty()) {
        query = query.filter(candidate::Column::Status.eq(status));
    }
    let rows =
        query.order_by_desc(candidate::Column::CreatedAt).order_by_desc(candidate::Column::Id).all(&db).await.map_err(KabiPayError::from).map_err(KabiPayError::into_graphql)?;
    let mut csv =
        "Candidate ID,Email,Status,Revision,First name,Last name,Created at,Updated at,Employee ID\r\n".to_owned();
    for row in rows {
        let values =
            [row.id.to_string(), row.email, row.status,
                    row.revision.to_string(),
                    row.answers.get("firstName").and_then(Value::as_str).unwrap_or_default().into(),
                    row.answers.get("lastName").and_then(Value::as_str).unwrap_or_default().into(),
                    row.created_at.to_rfc3339(), row.updated_at.to_rfc3339(),
                    row.employee_id.map(|v| v.to_string()).unwrap_or_default()];
        csv.push_str(&values.iter().map(|v|
                                csv_cell(v)).collect::<Vec<_>>().join(","));
        csv.push_str("\r\n");
    }
    Ok(csv)
}
#[cfg(test)]
mod tests {
    use super::*;
    use async_graphql::{EmptySubscription, Request, Schema};
    use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
    use std::collections::HashMap;
    use kabipay_common::subgraph::TenantId;
    #[tokio::test]
    async fn confirmation_rejects_bounded_employee_write_even_with_review_and_role_admin_all() {
        let tenant = Uuid::new_v4();
        let claims = ClientClaims {
            sub: Uuid::new_v4(), iss: CLIENT_JWT_ISSUER.into(), exp: i64::MAX, iat: 0,
            tenant_id: tenant, email: String::new(), employee_id: None,
            must_change_password: false, roles: vec![],
            permissions: vec!["prejoining:review".into(), "role:manage".into(), "employee:write".into()],
            permission_scopes: HashMap::from([
                ("prejoining:review".into(), "ALL".into()),
                ("role:manage".into(), "ALL".into()),
                ("employee:write".into(), "SELF".into()),
            ]),
            resource_scopes: HashMap::new(),
        };
        let query = format!("mutation {{ confirmPrejoiningJoined(input: {{ candidateId: \"{}\", revision: 0, employeeCode: \"E1\", dateOfJoining: \"2026-01-01\", username: \"candidate\", initialPassword: \"12345678\", roleIds: [] }}) {{ id }} }}", Uuid::new_v4());
        let schema = Schema::build(crate::resolvers::QueryRoot, crate::resolvers::MutationRoot, EmptySubscription).finish();
        let response = schema.execute(Request::new(query).data(TenantId(tenant)).data(claims)).await;
        assert_eq!(response.errors.len(), 1, "{response:?}");
        assert!(response.errors[0].message.contains("permission"), "{response:?}");
        assert!(!response.errors[0].message.contains("database"));
    }
    #[tokio::test]
    async fn dedicated_permissions_and_all_scope_are_checked_before_database_access() {
        for (query, permission, scope) in
            [("{ prejoiningCandidates { total } }", "employee:manage", "ALL"),
                    ("{ prejoiningCandidates { total } }", "prejoining:review",
                        "SELF"),
                    ("{ prejoiningConfig }", "prejoining:manage", "TEAM"),
                    ("mutation { savePrejoiningConfig(config: {}) }",
                        "prejoining:review", "ALL"),
                    ("mutation { invitePrejoining(email: \"a@example.test\", sendEmail: false) { privateUrl } }",
                        "prejoining:manage", "SELF")] {
            let tenant = Uuid::new_v4();
            let claims =
                ClientClaims {
                    sub: Uuid::new_v4(),
                    iss: CLIENT_JWT_ISSUER.into(),
                    exp: i64::MAX,
                    iat: 0,
                    tenant_id: tenant,
                    email: String::new(),
                    employee_id: None,
                    must_change_password: false,
                    roles: vec![],
                    permissions: vec![permission.into()],
                    permission_scopes: HashMap::from([(permission.into(),
                                    scope.into())]),
                    resource_scopes: HashMap::new(),
                };
            let schema =
                Schema::build(crate::resolvers::QueryRoot,
                        crate::resolvers::MutationRoot, EmptySubscription).finish();
            let response =
                schema.execute(Request::new(query).data(TenantId(tenant)).data(claims)).await;
            assert_eq!(response.errors.len(),1,"{response:?}");
            assert!(response.errors[0].message.contains("permission"),"{response:?}");
            assert!(!response.errors[0].message.contains("database"));
        }
    }
    #[test]
    fn exported_candidate_schema_never_exposes_invitation_digest_or_secret() {
        let schema =
            Schema::build(crate::resolvers::QueryRoot,
                        crate::resolvers::MutationRoot,
                        EmptySubscription).finish().sdl();
        let candidate =
            schema.split("type PrejoiningCandidate {").nth(1).unwrap().split('}').next().unwrap();
        assert!(!candidate.contains("digest"));
        assert!(!candidate.contains("privateUrl"));
        assert!(!candidate.contains("token"));
    }
    #[test]
    fn csv_escapes_formula_prefixes_quotes_and_newlines() {
        assert_eq!(csv_cell("\u{feff}=2+2"), "\"'\u{feff}=2+2\"");
        assert_eq!(csv_cell(" =2+2"),"\"' =2+2\"");
        assert_eq!(csv_cell("A\"B\nC"),"\"A\"\"B\nC\"");
    }
}
