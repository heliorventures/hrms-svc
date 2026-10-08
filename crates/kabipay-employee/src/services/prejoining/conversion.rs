//! One-transaction conversion into employee, account and private document records.
use super::*;
pub async fn confirm(db: &DatabaseConnection, tenant: Uuid, id: Uuid,
    revision: i32, actor: Uuid, mut data: NewEmployee,
    mut login: NewLoginAccount) -> Result<candidate::Model> {
    let tx = db.begin().await?;
    let row = locked(&tx, tenant, id).await?;
    if row.status == "JOINED" { return Ok(row); }
    check_revision(&row, revision)?;
    if row.status != "APPROVED" {
        return Err(conflict("PREJOINING_INVALID_STATE",
                    "Approve the submitted information before confirming joined."));
    }
    let config = decode_config(row.config.clone())?;
    validate_document_types(&tx, tenant, &config).await?;
    let answers = validate_answers(&config, row.answers.clone(), true)?;
    let text =
        |key: &str|
            answers.get(key).and_then(Value::as_str).filter(|v|
                        !v.is_empty()).map(str::to_owned);
    data.first_name =
        text("firstName").ok_or_else(|| validation("firstName required"))?;
    data.last_name =
        text("lastName").ok_or_else(|| validation("lastName required"))?;
    data.status = "ACTIVE".into();
    data.user_id = None;
    login.email = text("email");
    if data.employee_code.trim().is_empty() || data.employee_code.len() > 50 {
        return Err(validation("employeeCode must contain 1–50 characters"));
    }
    use kabipay_db_entities::tenant::d0006_org_hierarchy::{
        department, designation,
    };
    if let Some(id) = data.department_id {
        if department::Entity::find_by_id(id).filter(department::Column::TenantId.eq(tenant)).filter(department::Column::IsDeleted.eq(false)).lock_shared().one(&tx).await?.is_none()
            {
            return Err(validation("department is unavailable"));
        }
    }
    if let Some(id) = data.designation_id {
        if designation::Entity::find_by_id(id).filter(designation::Column::TenantId.eq(tenant)).filter(designation::Column::IsDeleted.eq(false)).lock_shared().one(&tx).await?.is_none()
            {
            return Err(validation("designation is unavailable"));
        }
    }
    if data.employment_type.as_ref().is_some_and(|v|
                v.chars().count() > 50 || v.chars().any(char::is_control)) {
        return Err(validation("invalid employmentType"));
    }
    let created =
        employee_service::create_with_login_in_transaction(&tx, tenant, data,
                    login).await?;
    let mut employee:
            kabipay_db_entities::tenant::d0007_employee_core::employee::ActiveModel =
        created.clone().into();
    employee.date_of_birth =
        Set(text("dateOfBirth").map(|v|
                                NaiveDate::parse_from_str(&v,
                                    "%Y-%m-%d")).transpose().map_err(|_|
                        validation("invalid dateOfBirth"))?);
    employee.gender = Set(text("gender"));
    employee.blood_group = Set(text("bloodGroup"));
    employee.nationality = Set(text("nationality"));
    employee.personal_phone = Set(text("personalPhone"));
    employee.current_address = Set(text("currentAddress"));
    employee.permanent_address = Set(text("permanentAddress"));
    employee.emergency_contact_name = Set(text("emergencyContactName"));
    employee.emergency_contact_phone = Set(text("emergencyContactPhone"));
    employee.emergency_contact_relation =
        Set(text("emergencyContactRelation"));
    employee.update(&tx).await?;
    let docs = documents(&tx, &row).await?;
    for requirement in config.documents.iter().filter(|d| d.required) {
        if !docs.iter().any(|d| d.requirement_id == requirement.id) {
            return Err(validation("required document missing"));
        }
    }
    let now = Utc::now();
    for metadata in docs {
        let doc = get_document(&tx, &row, metadata.id).await?;
        let file_id = Uuid::new_v4();
        file_storage::ActiveModel {
                        id: Set(file_id),
                        tenant_id: Set(tenant),
                        provider: Set("DATABASE".into()),
                        bucket: Set(None),
                        storage_path: Set(doc.id.to_string()),
                        original_filename: Set(Some(doc.filename.clone())),
                        mime_type: Set(Some(doc.mime_type.clone())),
                        file_size_bytes: Set(Some(doc.bytes.len() as i64)),
                        is_public: Set(false),
                        uploaded_by: Set(Some(actor)),
                        created_at: Set(now),
                        updated_at: Set(now),
                    }.insert(&tx).await?;
        employee_document::ActiveModel {
                        id: Set(Uuid::new_v4()),
                        tenant_id: Set(tenant),
                        employee_id: Set(created.id),
                        document_type_id: Set(doc.document_type_id),
                        file_storage_id: Set(Some(file_id)),
                        status: Set("APPROVED".into()),
                        expiry_date: Set(None),
                        workflow_instance_id: Set(None),
                        uploaded_at: Set(doc.created_at),
                        verified_by: Set(Some(actor)),
                        verified_at: Set(Some(now)),
                        is_deleted: Set(false),
                        deleted_at: Set(None),
                        deleted_by: Set(None),
                        created_at: Set(now),
                        updated_at: Set(now),
                    }.insert(&tx).await?;
        let mut model: document::ActiveModel = doc.into();
        model.file_storage_id = Set(Some(file_id));
        model.update(&tx).await?;
    }
    let mut model: candidate::ActiveModel = row.into();
    model.employee_id = Set(Some(created.id));
    model.status = Set("JOINED".into());
    model.invitation_digest = Set(None);
    model.revision = Set(revision + 1);
    model.updated_by = Set(Some(actor));
    model.updated_at = Set(now);
    let row = model.update(&tx).await?;
    record(&tx, &row, "CONFIRM_JOINED", Some(actor)).await?;
    tx.commit().await?;
    Ok(row)
}
