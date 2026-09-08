//! Passwordless invitation boundary. No employee claims or caller-supplied tenant headers.
use std::sync::Arc;
use axum::{
    body::{Body, Bytes},
    extract::{Path, State},
    http::{header, HeaderMap, StatusCode},
    response::{IntoResponse, Response},
    routing::{get, post},
    Json, Router,
};
use kabipay_common::{db::resolve_required_tenant_db, KabiPayError};
use sea_orm::DatabaseConnection;
use serde::Deserialize;
use serde_json::{json, Value};
use uuid::Uuid;
use crate::{EmployeeState, services::prejoining as service};
use kabipay_db_entities::tenant::d0080_prejoining::prejoining_candidate;
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Answers {
    revision: i32,
    answers: Value,
}
struct ApiError(KabiPayError);
impl From<KabiPayError> for ApiError {
    fn from(value: KabiPayError) -> Self { Self(value) }
}
fn protect(mut response: Response) -> Response {
    if response.status() == StatusCode::PAYLOAD_TOO_LARGE {
        response = (StatusCode::PAYLOAD_TOO_LARGE, Json(json!({"code":"PAYLOAD_TOO_LARGE","message":"Documents must be no larger than 10 MiB."}))).into_response();
    }
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL,
        "no-store".parse().expect("static header"));
    headers.insert("x-content-type-options",
        "nosniff".parse().expect("static header"));
    headers.insert("referrer-policy",
        "no-referrer".parse().expect("static header"));
    response
}
impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let error = self.0;
        let (status, code, message) =
            match error {
                KabiPayError::Unauthorised | KabiPayError::TenantNotFound(_) |
                    KabiPayError::TenantSuspended(_) =>
                    (StatusCode::UNAUTHORIZED, "INVITATION_UNAVAILABLE",
                        "This invitation has expired or is unavailable.".to_string()),
                KabiPayError::Validation(message) =>
                    (StatusCode::BAD_REQUEST, "VALIDATION_ERROR", message),
                KabiPayError::ConflictRule { code, message } =>
                    (StatusCode::CONFLICT, code, message),
                KabiPayError::NotFound { .. } =>
                    (StatusCode::NOT_FOUND, "NOT_FOUND",
                        "Requested document is unavailable.".to_string()),
                _ =>
                    (StatusCode::INTERNAL_SERVER_ERROR, "INTERNAL_ERROR",
                        "The form is temporarily unavailable. Try again.".to_string()),
            };
        protect((status,
                    Json(json!({
                                "code":code, "message":message
                            }))).into_response())
    }
}
pub fn routes() -> Router<Arc<EmployeeState>> {
    Router::new().route("/prejoining-api/form",
                        get(form).put(save)).route("/prejoining-api/form/submit",
                    post(submit)).route("/prejoining-api/form/documents/:id",
                post(upload).get(download).delete(delete)).layer(axum::extract::DefaultBodyLimit::max(service::MAX_DOCUMENT_BYTES)).layer(axum::middleware::map_response(|response:
                    Response| async { protect(response) }))
}
fn token(headers: &HeaderMap) -> Result<String, ApiError> {
    headers.get(header::AUTHORIZATION).and_then(|v|
                            v.to_str().ok()).and_then(|v|
                        v.strip_prefix("Bearer ")).filter(|v|
                    !v.is_empty()).map(str::to_string).ok_or_else(||
            ApiError(KabiPayError::Unauthorised))
}
async fn authorize(state: &EmployeeState, headers: &HeaderMap)
    ->
        Result<(DatabaseConnection, prejoining_candidate::Model, String),
        ApiError> {
    let token = token(headers)?;
    let (tenant, id) = service::verify_token(&token)?;
    let db =
        resolve_required_tenant_db(tenant, &state.ops, &state.cache,
                    &state.fallback).await?;
    let row =
        service::find(&db, tenant,
                            id).await?.ok_or(KabiPayError::Unauthorised)?;
    service::check_invitation(&row, &token, chrono::Utc::now())?;
    Ok((db, row, token))
}
fn revision(headers: &HeaderMap) -> Result<i32, ApiError> {
    headers.get("x-revision").and_then(|v|
                        v.to_str().ok()).and_then(|v|
                    v.parse().ok()).filter(|v|
                *v >=
                    0).ok_or_else(||
            service::validation("X-Revision is required").into())
}
fn parse_id(raw: &str) -> Result<Uuid, ApiError> {
    Uuid::parse_str(raw).map_err(|_|
            service::validation("invalid document ID").into())
}
fn parse_answers(body: &[u8]) -> Result<Answers, ApiError> {
    if body.len() > 64 * 1024 {
        return Err(service::validation("answers are too large").into());
    }
    serde_json::from_slice(body).map_err(|_|
            service::validation("invalid form JSON").into())
}
async fn form(State(state): State<Arc<EmployeeState>>, headers: HeaderMap)
    -> Result<Json<Value>, ApiError> {
    let (db, row, _) = authorize(&state, &headers).await?;
    Ok(Json(service::form(&db, &row).await?))
}
async fn save(State(state): State<Arc<EmployeeState>>, headers: HeaderMap,
    body: Bytes) -> Result<Json<Value>, ApiError> {
    save_inner(&state, &headers, &body, false).await
}
async fn submit(State(state): State<Arc<EmployeeState>>, headers: HeaderMap,
    body: Bytes) -> Result<Json<Value>, ApiError> {
    save_inner(&state, &headers, &body, true).await
}
async fn save_inner(state: &EmployeeState, headers: &HeaderMap, body: &[u8],
    submit: bool) -> Result<Json<Value>, ApiError> {
    let (db, row, token) = authorize(state, headers).await?;
    let input = parse_answers(body)?;
    Ok(Json(service::save_answers(&db, row.tenant_id, row.id, &token,
                        input.revision, input.answers, submit).await?))
}
async fn upload(State(state): State<Arc<EmployeeState>>,
    Path(requirement): Path<String>, headers: HeaderMap, body: Bytes)
    -> Result<Json<Value>, ApiError> {
    let (db, row, token) = authorize(&state, &headers).await?;
    let mime =
        headers.get(header::CONTENT_TYPE).and_then(|v|
                            v.to_str().ok()).ok_or_else(||
                        service::validation("Content-Type required"))?.to_string();
    let filename =
        headers.get("x-filename").and_then(|v|
                        v.to_str().ok()).ok_or_else(||
                    service::validation("X-Filename required"))?;
    let filename =
        urlencoding::decode(filename).map_err(|_|
                        service::validation("invalid filename encoding"))?.into_owned();
    Ok(Json(service::upload(&db, row.tenant_id, row.id, &token,
                        revision(&headers)?, parse_id(&requirement)?, filename,
                        mime, body.to_vec()).await?))
}
async fn delete(State(state): State<Arc<EmployeeState>>,
    Path(id): Path<String>, headers: HeaderMap)
    -> Result<Json<Value>, ApiError> {
    let (db, row, token) = authorize(&state, &headers).await?;
    Ok(Json(service::delete_document(&db, row.tenant_id, row.id, &token,
                        revision(&headers)?, parse_id(&id)?).await?))
}
async fn download(State(state): State<Arc<EmployeeState>>,
    Path(id): Path<String>, headers: HeaderMap)
    -> Result<Response, ApiError> {
    let (db, row, _) = authorize(&state, &headers).await?;
    let document = service::get_document(&db, &row, parse_id(&id)?).await?;
    let response =
        Response::builder().header(header::CONTENT_TYPE,
                            document.mime_type).header(header::CONTENT_DISPOSITION,
                        format!("attachment; filename*=UTF-8''{}",urlencoding::encode(&document.filename))).body(Body::from(document.bytes)).map_err(|_|
                    KabiPayError::Internal("document response failed".into()))?;
    Ok(protect(response))
}
