//! Locked candidate persistence and immutable revision snapshots.
use super::*;
pub async fn get_config<C: ConnectionTrait>(db: &C, tenant: Uuid)
    -> Result<Value> {
    Ok(config::Entity::find_by_id(tenant).one(db).await?.map(|r|
                    r.config).unwrap_or_else(|| json!(Config::default())))
}
pub async fn save_config(db: &DatabaseConnection, tenant: Uuid, actor: Uuid,
    value: Value) -> Result<Value> {
    let config = decode_config(value)?;
    validate_document_types(db, tenant, &config).await?;
    let value = json!(config);
    let model =
        config::ActiveModel {
            tenant_id: Set(tenant),
            config: Set(value.clone()),
            updated_by: Set(actor),
            updated_at: Set(Utc::now()),
        };
    config::Entity::insert(model).on_conflict(sea_orm::sea_query::OnConflict::column(config::Column::TenantId).update_columns([config::Column::Config,
                                    config::Column::UpdatedBy,
                                    config::Column::UpdatedAt]).to_owned()).exec(db).await?;
    Ok(value)
}
pub(super) async fn validate_document_types<C: ConnectionTrait>(db: &C,
    tenant: Uuid, config: &Config) -> Result<()> {
    for requirement in &config.documents {
        if document_type::Entity::find_by_id(requirement.document_type_id).filter(document_type::Column::TenantId.eq(tenant)).filter(document_type::Column::IsDeleted.eq(false)).one(db).await?.is_none()
            {
            return Err(validation("document type is unavailable"));
        }
    }
    Ok(())
}
pub async fn find<C: ConnectionTrait>(db: &C, tenant: Uuid, id: Uuid)
    -> Result<Option<candidate::Model>> {
    Ok(candidate::Entity::find_by_id(id).filter(candidate::Column::TenantId.eq(tenant)).one(db).await?)
}
pub async fn locked(db: &DatabaseTransaction, tenant: Uuid, id: Uuid)
    -> Result<candidate::Model> {
    candidate::Entity::find_by_id(id).filter(candidate::Column::TenantId.eq(tenant)).lock_exclusive().one(db).await?.ok_or_else(||
            Error::NotFound { entity: "candidate", id: id.to_string() })
}
#[derive(sea_orm::FromQueryResult)]
pub struct DocumentMetadata {
    pub id: Uuid,
    pub requirement_id: Uuid,
    pub filename: String,
    pub mime_type: String,
    pub size_bytes: i32,
}
pub async fn documents<C: ConnectionTrait>(db: &C, row: &candidate::Model)
    -> Result<Vec<DocumentMetadata>> {
    Ok(document::Entity::find().select_only().columns([document::Column::Id,
                                                        document::Column::RequirementId, document::Column::Filename,
                                                        document::Column::MimeType]).expr_as(sea_orm::sea_query::Expr::cust("octet_length(bytes)"),
                                            "size_bytes").filter(document::Column::TenantId.eq(row.tenant_id)).filter(document::Column::CandidateId.eq(row.id)).filter(document::Column::IsCurrent.eq(true)).order_by_asc(document::Column::CreatedAt).into_model::<DocumentMetadata>().all(db).await?)
}
pub fn document_metadata(row: &DocumentMetadata) -> Value {
    json!({
            "id":row.id, "requirementId":row.requirement_id,
            "filename":row.filename, "mimeType":row.mime_type,
            "sizeBytes":row.size_bytes
        })
}
pub async fn form<C: ConnectionTrait>(db: &C, row: &candidate::Model)
    -> Result<Value> {
    Ok(json!({
                "status":row.status, "revision":row.revision,
                "config":row.config, "answers":row.answers,
                "feedback":row.feedback,
                "documents":documents(db,row).await?.iter().map(document_metadata).collect::<Vec<_>>(),
                "expiresAt":row.expires_at
            }))
}
pub(super) async fn record<C: ConnectionTrait>(db: &C, row: &candidate::Model,
    action: &str, actor: Option<Uuid>) -> Result<()> {
    event::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    tenant_id: Set(row.tenant_id),
                    candidate_id: Set(row.id),
                    revision: Set(row.revision),
                    action: Set(action.into()),
                    snapshot: Set(form(db, row).await?),
                    actor_id: Set(actor),
                    created_at: Set(Utc::now()),
                }.insert(db).await?;
    Ok(())
}
pub async fn invite(db: &DatabaseConnection, tenant: Uuid, actor: Uuid,
    email: &str) -> Result<(candidate::Model, String)> {
    let email = normalize_email(email)?;
    let config = decode_config(get_config(db, tenant).await?)?;
    validate_document_types(db, tenant, &config).await?;
    let id = Uuid::new_v4();
    let token = issue_token(tenant, id)?;
    let url = private_url(&token)?;
    let now = Utc::now();
    let tx = db.begin().await?;
    let row =
        candidate::ActiveModel {
                        id: Set(id),
                        tenant_id: Set(tenant),
                        email: Set(email.clone()),
                        status: Set("DRAFT".into()),
                        revision: Set(0),
                        config: Set(json!(config)),
                        answers: Set(json!({ "email":email })),
                        feedback: Set(None),
                        invitation_digest: Set(Some(digest(&token))),
                        expires_at: Set(Some(now +
                                    Duration::hours(config.expiry_hours))),
                        employee_id: Set(None),
                        created_by: Set(actor),
                        updated_by: Set(Some(actor)),
                        created_at: Set(now),
                        updated_at: Set(now),
                    }.insert(&tx).await?;
    record(&tx, &row, "INVITE", Some(actor)).await?;
    tx.commit().await?;
    Ok((row, url))
}
pub async fn reissue(db: &DatabaseConnection, tenant: Uuid, id: Uuid,
    revision: i32, actor: Uuid) -> Result<(candidate::Model, String)> {
    let token = issue_token(tenant, id)?;
    let url = private_url(&token)?;
    let tx = db.begin().await?;
    let row = locked(&tx, tenant, id).await?;
    check_revision(&row, revision)?;
    if matches!(row.status.as_str(),"JOINED"|"CANCELLED") {
        return Err(conflict("PREJOINING_INVALID_STATE",
                    "Cannot reissue a closed invitation."));
    }
    let config = decode_config(row.config.clone())?;
    let mut model: candidate::ActiveModel = row.into();
    model.invitation_digest = Set(Some(digest(&token)));
    model.expires_at =
        Set(Some(Utc::now() + Duration::hours(config.expiry_hours)));
    model.revision = Set(revision + 1);
    model.updated_at = Set(Utc::now());
    model.updated_by = Set(Some(actor));
    let row = model.update(&tx).await?;
    record(&tx, &row, "REISSUE", Some(actor)).await?;
    tx.commit().await?;
    Ok((row, url))
}
pub async fn review(db: &DatabaseConnection, tenant: Uuid, id: Uuid,
    revision: i32, actor: Uuid, action: &str, feedback: Option<String>)
    -> Result<candidate::Model> {
    let tx = db.begin().await?;
    let row = locked(&tx, tenant, id).await?;
    check_revision(&row, revision)?;
    let status = check_transition(&row.status, action)?;
    if action == "REQUEST_CHANGES" &&
            feedback.as_ref().is_none_or(|v|
                    v.trim().is_empty() || v.len() > 4000) {
        return Err(validation("correction feedback is required, maximum 4000 characters"));
    }
    let mut model: candidate::ActiveModel = row.into();
    model.status = Set(status.into());
    model.revision = Set(revision + 1);
    model.updated_at = Set(Utc::now());
    model.updated_by = Set(Some(actor));
    if action == "REQUEST_CHANGES" {
        model.feedback = Set(feedback);
        model.invitation_digest = Set(None);
    }
    if action == "CANCEL" { model.invitation_digest = Set(None); }
    let row = model.update(&tx).await?;
    record(&tx, &row, action, Some(actor)).await?;
    tx.commit().await?;
    Ok(row)
}
pub async fn save_answers(db: &DatabaseConnection, tenant: Uuid, id: Uuid,
    token: &str, revision: i32, answers: Value, submit: bool)
    -> Result<Value> {
    let tx = db.begin().await?;
    let row = locked(&tx, tenant, id).await?;
    check_invitation(&row, token, Utc::now())?;
    check_revision(&row, revision)?;
    check_editable(&row.status)?;
    let config = decode_config(row.config.clone())?;
    let answers = validate_answers(&config, answers, submit)?;
    if submit {
        let docs = documents(&tx, &row).await?;
        for requirement in config.documents.iter().filter(|d| d.required) {
            if !docs.iter().any(|d| d.requirement_id == requirement.id) {
                return Err(validation(format!("{} document is required",requirement.label)));
            }
        }
    }
    let mut model: candidate::ActiveModel = row.into();
    model.answers = Set(answers);
    model.revision = Set(revision + 1);
    model.updated_by = Set(None);
    model.updated_at = Set(Utc::now());
    if submit { model.status = Set("SUBMITTED".into()); }
    let row = model.update(&tx).await?;
    record(&tx, &row, if submit { "SUBMIT" } else { "SAVE_DRAFT" },
                None).await?;
    let value = form(&tx, &row).await?;
    tx.commit().await?;
    Ok(value)
}
pub async fn upload(db: &DatabaseConnection, tenant: Uuid, id: Uuid,
    token: &str, revision: i32, requirement_id: Uuid, filename: String,
    mime: String, bytes: Vec<u8>) -> Result<Value> {
    validate_document(&mime, &bytes)?;
    if filename.trim().is_empty() || filename.len() > 180 ||
            filename.chars().any(|c|
                    c.is_control() || matches!(c,'/'|'\\'|'"')) {
        return Err(validation("invalid filename"));
    }
    let tx = db.begin().await?;
    let row = locked(&tx, tenant, id).await?;
    check_invitation(&row, token, Utc::now())?;
    check_revision(&row, revision)?;
    check_editable(&row.status)?;
    let config = decode_config(row.config.clone())?;
    let requirement =
        config.documents.iter().find(|r|
                        r.id ==
                            requirement_id).ok_or_else(||
                    validation("unknown document requirement"))?;
    let usage =
        tx.query_one(sea_orm::Statement::from_sql_and_values(sea_orm::DbBackend::Postgres,
                                        "SELECT COALESCE(SUM(octet_length(bytes)),0)::BIGINT AS total FROM prejoining_document WHERE tenant_id=$1 AND candidate_id=$2",
                                        [tenant.into(),
                                                id.into()])).await?.ok_or_else(||
                            Error::Internal("document usage unavailable".into()))?.try_get::<i64>("",
                "total")?;
    if usage + bytes.len() as i64 > 512 * 1024 * 1024 {
        return Err(validation("This invitation has reached its 512 MiB document history limit. Contact HR for assistance."));
    }
    document::Entity::update_many().col_expr(document::Column::IsCurrent,
                                    sea_orm::sea_query::Expr::value(false)).filter(document::Column::TenantId.eq(tenant)).filter(document::Column::CandidateId.eq(id)).filter(document::Column::RequirementId.eq(requirement_id)).filter(document::Column::IsCurrent.eq(true)).exec(&tx).await?;
    document::ActiveModel {
                    id: Set(Uuid::new_v4()),
                    tenant_id: Set(tenant),
                    candidate_id: Set(id),
                    requirement_id: Set(requirement_id),
                    document_type_id: Set(requirement.document_type_id),
                    filename: Set(filename),
                    mime_type: Set(mime),
                    bytes: Set(bytes),
                    is_current: Set(true),
                    file_storage_id: Set(None),
                    created_at: Set(Utc::now()),
                }.insert(&tx).await?;
    let mut model: candidate::ActiveModel = row.into();
    model.revision = Set(revision + 1);
    model.updated_by = Set(None);
    model.updated_at = Set(Utc::now());
    let row = model.update(&tx).await?;
    record(&tx, &row, "UPLOAD", None).await?;
    let value = form(&tx, &row).await?;
    tx.commit().await?;
    Ok(value)
}
pub async fn delete_document(db: &DatabaseConnection, tenant: Uuid, id: Uuid,
    token: &str, revision: i32, document_id: Uuid) -> Result<Value> {
    let tx = db.begin().await?;
    let row = locked(&tx, tenant, id).await?;
    check_invitation(&row, token, Utc::now())?;
    check_revision(&row, revision)?;
    check_editable(&row.status)?;
    let doc = get_document(&tx, &row, document_id).await?;
    let mut doc: document::ActiveModel = doc.into();
    doc.is_current = Set(false);
    doc.update(&tx).await?;
    let mut model: candidate::ActiveModel = row.into();
    model.revision = Set(revision + 1);
    model.updated_by = Set(None);
    model.updated_at = Set(Utc::now());
    let row = model.update(&tx).await?;
    record(&tx, &row, "DELETE_DOCUMENT", None).await?;
    let value = form(&tx, &row).await?;
    tx.commit().await?;
    Ok(value)
}
pub async fn get_document<C: ConnectionTrait>(db: &C, row: &candidate::Model,
    id: Uuid) -> Result<document::Model> {
    document::Entity::find_by_id(id).filter(document::Column::TenantId.eq(row.tenant_id)).filter(document::Column::CandidateId.eq(row.id)).filter(document::Column::IsCurrent.eq(true)).one(db).await?.ok_or_else(||
            Error::NotFound { entity: "document", id: "requested".into() })
}
