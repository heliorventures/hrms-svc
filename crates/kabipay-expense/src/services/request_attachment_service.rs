//! Resolve evidence through its authorized parent request, never a caller-selected file ID.

use async_graphql::SimpleObject;
use base64::{engine::general_purpose::STANDARD, Engine};
use kabipay_common::client_data_scope::EmployeeScopeFilter;
use kabipay_common::private_file_reader::{local_file_root, read_stored_file_bytes};
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0015_expense::expense;
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use kabipay_db_entities::tenant::d0033_travel_request::travel_request;
use sea_orm::{ColumnTrait, DatabaseConnection, EntityTrait, QueryFilter};
use uuid::Uuid;

#[derive(SimpleObject)]
pub struct RequestAttachment {
    pub file_name: String,
    pub mime_type: String,
    pub content_base64: String,
}

pub enum RequestKind {
    Expense,
    Travel,
}

pub async fn load_attachment(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    request_id: Uuid,
    kind: RequestKind,
    scope: &EmployeeScopeFilter,
) -> KabiPayResult<Option<RequestAttachment>> {
    let (employee_id, file_id) = match kind {
        RequestKind::Expense => {
            let row = expense::Entity::find_by_id(request_id)
                .filter(expense::Column::TenantId.eq(tenant_id))
                .filter(expense::Column::IsDeleted.eq(false))
                .one(db)
                .await?
                .ok_or_else(request_unavailable)?;
            (row.employee_id, row.receipt_file_storage_id)
        }
        RequestKind::Travel => {
            let row = travel_request::Entity::find_by_id(request_id)
                .filter(travel_request::Column::TenantId.eq(tenant_id))
                .one(db)
                .await?
                .ok_or_else(request_unavailable)?;
            (row.employee_id, row.supporting_file_storage_id)
        }
    };
    if !scope.allows_employee(employee_id) {
        return Err(request_unavailable());
    }
    let Some(file_id) = file_id else {
        return Ok(None);
    };
    let file = file_storage::Entity::find_by_id(file_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(request_unavailable)?;
    if file.is_public || !matches!(file.provider.as_str(), "LOCAL" | "S3") {
        return Err(KabiPayError::Validation(
            "attachment is not a private tenant file".into(),
        ));
    }
    if !file
        .file_size_bytes
        .is_some_and(|size| size > 0 && size <= 6 * 1024 * 1024)
    {
        return Err(KabiPayError::Validation(
            "attachment size is unavailable or exceeds 6 MB".into(),
        ));
    }
    let bytes = read_stored_file_bytes(db, &local_file_root(), &file).await?;
    if bytes.is_empty() || bytes.len() > 6 * 1024 * 1024 {
        return Err(KabiPayError::Validation(
            "attachment is empty or exceeds 6 MB".into(),
        ));
    }
    Ok(Some(RequestAttachment {
        file_name: file
            .original_filename
            .unwrap_or_else(|| "supporting-file".into()),
        mime_type: file
            .mime_type
            .unwrap_or_else(|| "application/octet-stream".into()),
        content_base64: STANDARD.encode(bytes),
    }))
}

fn request_unavailable() -> KabiPayError {
    KabiPayError::Forbidden("request or supporting file is unavailable".into())
}
