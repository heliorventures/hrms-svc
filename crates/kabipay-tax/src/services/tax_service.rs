//! Tenant-scoped SeaORM queries and commands for tax configuration, slabs, and employee computations.

use chrono::Utc;
use kabipay_common::{
    client_data_scope::{
        resolve_employee_scope_filter_with_connection, EmployeeScopeFilter,
    },
    context::{ClientViewerEmployee, ScopeType},
    KabiPayError, KabiPayResult,
};
use kabipay_db_entities::tenant::d0007_employee_core::employee;
use kabipay_db_entities::tenant::d0013_tax_statutory::{
    tax_computation, tax_configuration_version, tax_section_definition, tax_slab,
};
use kabipay_db_entities::tenant::d0029_file_storage::file_storage;
use kabipay_db_entities::tenant::d0027_communication_audit::notification;
use kabipay_db_entities::tenant::d0031_tax_proof::tax_proof_line;
use rust_decimal::Decimal;
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DatabaseConnection, DatabaseTransaction,
    EntityTrait, QueryFilter, QueryOrder, QuerySelect, Select, Set, TransactionTrait,
};
use sea_orm::sea_query::LockType;
use sea_orm::PaginatorTrait;
use std::str::FromStr;
use uuid::Uuid;

pub async fn list_configurations(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    active_only: bool,
    limit: u64,
) -> KabiPayResult<Vec<tax_configuration_version::Model>> {
    let limit = limit.clamp(1, 40);
    let mut q = tax_configuration_version::Entity::find()
        .filter(tax_configuration_version::Column::TenantId.eq(tenant_id));
    if active_only {
        q = q.filter(tax_configuration_version::Column::IsActive.eq(true));
    }
    q.order_by_desc(tax_configuration_version::Column::FiscalYear)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_slabs(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<tax_slab::Model>> {
    let limit = limit.clamp(1, 200);
    tax_slab::Entity::find()
        .filter(tax_slab::Column::TenantId.eq(tenant_id))
        .order_by_asc(tax_slab::Column::IncomeFrom)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

pub async fn list_computations(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    limit: u64,
) -> KabiPayResult<Vec<tax_computation::Model>> {
    let limit = limit.clamp(1, 100);
    tax_computation::Entity::find()
        .filter(tax_computation::Column::TenantId.eq(tenant_id))
        .filter(tax_computation::Column::EmployeeId.eq(employee_id))
        .order_by_desc(tax_computation::Column::FiscalYear)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Create or update the row keyed by (tenant, employee, tax config version, fiscal year).
pub async fn upsert_tax_computation(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    tax_config_version_id: Uuid,
    fiscal_year: i32,
    tax_regime_chosen: Option<String>,
    gross_income: Option<Decimal>,
    total_deductions: Option<Decimal>,
    taxable_income: Option<Decimal>,
    final_tax: Option<Decimal>,
    tds_per_month: Option<Decimal>,
) -> KabiPayResult<tax_computation::Model> {
    let _ver = tax_configuration_version::Entity::find()
        .filter(tax_configuration_version::Column::Id.eq(tax_config_version_id))
        .filter(tax_configuration_version::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tax_configuration_version",
            id: tax_config_version_id.to_string(),
        })?;
    if _ver.fiscal_year != fiscal_year {
        return Err(KabiPayError::Validation(
            "fiscalYear does not match the selected tax configuration version".into(),
        ));
    }
    let existing = tax_computation::Entity::find()
        .filter(tax_computation::Column::TenantId.eq(tenant_id))
        .filter(tax_computation::Column::EmployeeId.eq(employee_id))
        .filter(tax_computation::Column::TaxConfigVersionId.eq(tax_config_version_id))
        .filter(tax_computation::Column::FiscalYear.eq(fiscal_year))
        .one(db)
        .await?;
    let now = Utc::now();
    if let Some(row) = existing {
        let id = row.id;
        let mut am: tax_computation::ActiveModel = row.into();
        am.tax_regime_chosen = Set(tax_regime_chosen);
        am.gross_income = Set(gross_income);
        am.total_deductions = Set(total_deductions);
        am.taxable_income = Set(taxable_income);
        am.final_tax = Set(final_tax);
        am.tds_per_month = Set(tds_per_month);
        am.computed_at = Set(now);
        am.update(db).await?;
        tax_computation::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("updated tax_computation not found".into()))
    } else {
        let id = Uuid::new_v4();
        let am = tax_computation::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            tax_config_version_id: Set(tax_config_version_id),
            fiscal_year: Set(fiscal_year),
            tax_regime_chosen: Set(tax_regime_chosen),
            gross_income: Set(gross_income),
            total_deductions: Set(total_deductions),
            taxable_income: Set(taxable_income),
            final_tax: Set(final_tax),
            tds_per_month: Set(tds_per_month),
            computed_at: Set(now),
        };
        am.insert(db).await?;
        tax_computation::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("inserted tax_computation not found".into()))
    }
}

pub fn opt_decimal(s: &Option<String>) -> KabiPayResult<Option<Decimal>> {
    match s {
        None => Ok(None),
        Some(t) if t.trim().is_empty() => Ok(None),
        Some(t) => Decimal::from_str(t.trim())
            .map(Some)
            .map_err(|_| KabiPayError::Validation("invalid decimal string in tax fields".into())),
    }
}

const PROOF_PENDING: &str = "PENDING";
const PROOF_APPROVED: &str = "APPROVED";
const PROOF_REJECTED: &str = "REJECTED";
const TAX_PROOF_ALLOWED_MIME_TYPES: &[&str] = &["application/pdf", "image/jpeg", "image/png"];

/// If the tenant maintains an active **`tax_section_definition`** catalogue, `section_code` must exist there.
pub async fn enforce_proof_section_catalog_match(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    section_normalized: &str,
    claimed_amount_for_cap_check: Decimal,
) -> KabiPayResult<()> {
    let n = tax_section_definition::Entity::find()
        .filter(tax_section_definition::Column::TenantId.eq(tenant_id))
        .filter(tax_section_definition::Column::IsActive.eq(true))
        .count(db)
        .await
        .map_err(KabiPayError::from)?;
    if n == 0 {
        return Ok(());
    }
    let sc = section_normalized.trim().to_uppercase();
    let def = tax_section_definition::Entity::find()
        .filter(tax_section_definition::Column::TenantId.eq(tenant_id))
        .filter(tax_section_definition::Column::SectionCode.eq(&sc))
        .filter(tax_section_definition::Column::IsActive.eq(true))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;
    let Some(row) = def else {
        return Err(KabiPayError::Validation(format!(
            "sectionCode `{sc}` is not in your tenant tax section catalogue — ask HR to add it or correct the code.",
        )));
    };
    if let Some(cap) = row.max_deduction_amount {
        if claimed_amount_for_cap_check > cap {
            return Err(KabiPayError::Validation(format!(
                "claimed amount exceeds configured cap {cap} for section `{sc}`",
            )));
        }
    }
    Ok(())
}

pub async fn list_tax_proof_lines(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    tax_config_version_id: Option<Uuid>,
    fiscal_year: Option<i32>,
) -> KabiPayResult<Vec<tax_proof_line::Model>> {
    let mut q = tax_proof_line::Entity::find()
        .filter(tax_proof_line::Column::TenantId.eq(tenant_id))
        .filter(tax_proof_line::Column::EmployeeId.eq(employee_id));
    if let Some(v) = tax_config_version_id {
        q = q.filter(tax_proof_line::Column::TaxConfigVersionId.eq(v));
    }
    if let Some(y) = fiscal_year {
        q = q.filter(tax_proof_line::Column::FiscalYear.eq(y));
    }
    q.order_by_asc(tax_proof_line::Column::SectionCode)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Employee submits or updates a proof line (e.g. 80C, HRA) — goes to **PENDING** until approved.
/// Only **APPROVED** lines roll into `tax_computation.total_deductions` (see
/// `recompute_total_deductions_from_approved_proofs`).
pub async fn submit_tax_proof_line(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    submitting_user_id: Uuid,
    tax_config_version_id: Uuid,
    fiscal_year: i32,
    section_code: String,
    declared_amount: Decimal,
    actual_amount: Decimal,
    file_storage_id: Uuid,
) -> KabiPayResult<tax_proof_line::Model> {
    if declared_amount < Decimal::ZERO || actual_amount < Decimal::ZERO {
        return Err(KabiPayError::Validation(
            "declared and actual amounts must be non-negative".into(),
        ));
    }
    let sc = section_code.trim().to_string();
    if sc.is_empty() {
        return Err(KabiPayError::Validation("sectionCode is required".into()));
    }
    enforce_proof_section_catalog_match(db, tenant_id, &sc, declared_amount.max(actual_amount))
        .await?;
    assert_tax_proof_file(db, tenant_id, file_storage_id, Some(submitting_user_id)).await?;

    let _ver = tax_configuration_version::Entity::find()
        .filter(tax_configuration_version::Column::Id.eq(tax_config_version_id))
        .filter(tax_configuration_version::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tax_configuration_version",
            id: tax_config_version_id.to_string(),
        })?;
    if _ver.fiscal_year != fiscal_year {
        return Err(KabiPayError::Validation(
            "fiscalYear does not match the tax configuration version".into(),
        ));
    }

    let existing = tax_proof_line::Entity::find()
        .filter(tax_proof_line::Column::TenantId.eq(tenant_id))
        .filter(tax_proof_line::Column::EmployeeId.eq(employee_id))
        .filter(tax_proof_line::Column::TaxConfigVersionId.eq(tax_config_version_id))
        .filter(tax_proof_line::Column::SectionCode.eq(&sc))
        .one(db)
        .await?;

    let now = Utc::now();
    let out = if let Some(row) = existing {
        let id = row.id;
        let mut am: tax_proof_line::ActiveModel = row.into();
        am.declared_amount = Set(declared_amount);
        am.actual_amount = Set(actual_amount);
        am.file_storage_id = Set(Some(file_storage_id));
        am.status = Set(PROOF_PENDING.into());
        am.rejection_reason = Set(None);
        am.approved_by = Set(None);
        am.submitted_at = Set(now);
        am.fiscal_year = Set(fiscal_year);
        am.update(db).await?;
        tax_proof_line::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("updated tax_proof_line not found".into()))?
    } else {
        let id = Uuid::new_v4();
        let am = tax_proof_line::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            tax_config_version_id: Set(tax_config_version_id),
            fiscal_year: Set(fiscal_year),
            section_code: Set(sc),
            declared_amount: Set(declared_amount),
            actual_amount: Set(actual_amount),
            file_storage_id: Set(Some(file_storage_id)),
            status: Set(PROOF_PENDING.into()),
            rejection_reason: Set(None),
            approved_by: Set(None),
            submitted_at: Set(now),
            created_at: Set(now),
            updated_at: Set(now),
        };
        am.insert(db).await?;
        tax_proof_line::Entity::find_by_id(id)
            .one(db)
            .await?
            .ok_or_else(|| KabiPayError::Internal("inserted tax_proof_line not found".into()))?
    };

    recompute_total_deductions_from_approved_proofs(
        db,
        tenant_id,
        employee_id,
        tax_config_version_id,
        fiscal_year,
    )
    .await?;

    Ok(out)
}

fn tax_proof_for_decision_query(
    tenant_id: Uuid,
    line_id: Uuid,
) -> Select<tax_proof_line::Entity> {
    tax_proof_line::Entity::find()
        .filter(tax_proof_line::Column::Id.eq(line_id))
        .filter(tax_proof_line::Column::TenantId.eq(tenant_id))
        .lock(LockType::Update)
}

fn require_pending_tax_proof_status(status: &str) -> KabiPayResult<()> {
    if status != PROOF_PENDING {
        return Err(KabiPayError::Conflict(
            "tax proof is no longer pending; refresh before taking another action".into(),
        ));
    }
    Ok(())
}

async fn lock_pending_tax_proof(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    line_id: Uuid,
) -> KabiPayResult<tax_proof_line::Model> {
    let model = tax_proof_for_decision_query(tenant_id, line_id)
        .one(txn)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tax_proof_line",
            id: line_id.to_string(),
        })?;
    require_pending_tax_proof_status(&model.status)?;
    Ok(model)
}

pub(crate) fn require_tax_approval_target(
    target_scope: &EmployeeScopeFilter,
    approver_employee_id: Uuid,
    target_employee_id: Uuid,
) -> KabiPayResult<()> {
    if approver_employee_id == target_employee_id {
        return Err(KabiPayError::Forbidden(
            "tax proof self-approval is not allowed".into(),
        ));
    }
    if !target_scope.allows_employee(target_employee_id) {
        return Err(KabiPayError::Forbidden(
            "tax:approve scope does not include target employee".into(),
        ));
    }
    Ok(())
}

async fn require_tax_approval_target_in_txn(
    txn: &DatabaseTransaction,
    tenant_id: Uuid,
    scope: ScopeType,
    approver_employee_id: Uuid,
    target_employee_id: Uuid,
) -> KabiPayResult<()> {
    let target_scope = resolve_employee_scope_filter_with_connection(
        txn,
        tenant_id,
        scope,
        Some(ClientViewerEmployee {
            employee_id: approver_employee_id,
            department_id: None,
        }),
    )
    .await?;
    require_tax_approval_target(
        &target_scope,
        approver_employee_id,
        target_employee_id,
    )
}

pub async fn approve_tax_proof_line(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    line_id: Uuid,
    scope: ScopeType,
    approver_employee_id: Uuid,
    approver_user_id: Uuid,
) -> KabiPayResult<tax_proof_line::Model> {
    let txn = db.begin().await?;
    let decision = async {
        let model = lock_pending_tax_proof(&txn, tenant_id, line_id).await?;
        require_tax_approval_target_in_txn(
            &txn,
            tenant_id,
            scope,
            approver_employee_id,
            model.employee_id,
        )
        .await?;
        let file_storage_id = model.file_storage_id.ok_or_else(|| {
            KabiPayError::Validation("tax proof file is required before approval".into())
        })?;
        assert_tax_proof_file(&txn, tenant_id, file_storage_id, None).await?;
        let tid = model.tax_config_version_id;
        let eid = model.employee_id;
        let fy = model.fiscal_year;
        let mut am: tax_proof_line::ActiveModel = model.into();
        am.status = Set(PROOF_APPROVED.into());
        am.rejection_reason = Set(None);
        am.approved_by = Set(Some(approver_user_id));
        am.update(&txn).await?;
        let out = tax_proof_line::Entity::find_by_id(line_id)
            .filter(tax_proof_line::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Internal("updated tax_proof_line not found".into()))?;
        recompute_total_deductions_from_approved_proofs(&txn, tenant_id, eid, tid, fy).await?;
        Ok::<_, KabiPayError>(out)
    }
    .await;
    let out = match decision {
        Ok(out) => {
            txn.commit().await?;
            out
        }
        Err(error) => {
            txn.rollback().await?;
            return Err(error);
        }
    };
    tax_proof_notify_employee(
        db,
        tenant_id,
        out.employee_id,
        "Tax proof approved",
        &format!(
            "Your {} deduction proof (FY {}) was approved and counts toward year-end tax.",
            out.section_code, out.fiscal_year
        ),
    )
    .await;
    Ok(out)
}

async fn assert_tax_proof_file<C>(
    db: &C,
    tenant_id: Uuid,
    file_storage_id: Uuid,
    required_uploader_user_id: Option<Uuid>,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    let file = file_storage::Entity::find_by_id(file_storage_id)
        .filter(file_storage::Column::TenantId.eq(tenant_id))
        .one(db)
        .await?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "file_storage",
            id: file_storage_id.to_string(),
        })?;

    if let Some(expected_uploader) = required_uploader_user_id {
        if file.uploaded_by != Some(expected_uploader) {
            return Err(KabiPayError::Validation(
                "tax proof file must be uploaded by the submitting user".into(),
            ));
        }
    }

    match file.file_size_bytes {
        Some(size) if size > 0 => {}
        _ => {
            return Err(KabiPayError::Validation(
                "tax proof file must not be empty".into(),
            ));
        }
    }

    let mime = file
        .mime_type
        .as_deref()
        .map(str::trim)
        .unwrap_or_default()
        .to_ascii_lowercase();
    if !TAX_PROOF_ALLOWED_MIME_TYPES.contains(&mime.as_str()) {
        return Err(KabiPayError::Validation(
            "tax proof file must be a PDF, JPG, or PNG file".into(),
        ));
    }

    Ok(())
}

pub async fn reject_tax_proof_line(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    line_id: Uuid,
    scope: ScopeType,
    approver_employee_id: Uuid,
    rejection_reason: Option<String>,
) -> KabiPayResult<tax_proof_line::Model> {
    let txn = db.begin().await?;
    let decision = async {
        let model = lock_pending_tax_proof(&txn, tenant_id, line_id).await?;
        require_tax_approval_target_in_txn(
            &txn,
            tenant_id,
            scope,
            approver_employee_id,
            model.employee_id,
        )
        .await?;
        let tid = model.tax_config_version_id;
        let eid = model.employee_id;
        let fy = model.fiscal_year;
        let mut am: tax_proof_line::ActiveModel = model.into();
        am.status = Set(PROOF_REJECTED.into());
        am.rejection_reason = Set(rejection_reason);
        am.approved_by = Set(None);
        am.update(&txn).await?;
        let out = tax_proof_line::Entity::find_by_id(line_id)
            .filter(tax_proof_line::Column::TenantId.eq(tenant_id))
            .one(&txn)
            .await?
            .ok_or_else(|| KabiPayError::Internal("updated tax_proof_line not found".into()))?;
        recompute_total_deductions_from_approved_proofs(&txn, tenant_id, eid, tid, fy).await?;
        Ok::<_, KabiPayError>(out)
    }
    .await;
    let out = match decision {
        Ok(out) => {
            txn.commit().await?;
            out
        }
        Err(error) => {
            txn.rollback().await?;
            return Err(error);
        }
    };
    let msg = match &out.rejection_reason {
        Some(s) if !s.is_empty() => format!(
            "Your {} proof (FY {}) was rejected. Reason: {s}",
            out.section_code, out.fiscal_year
        ),
        _ => format!(
            "Your {} proof (FY {}) was rejected. It will not count toward tax deductions until resubmitted and approved.",
            out.section_code, out.fiscal_year
        ),
    };
    tax_proof_notify_employee(
        db,
        tenant_id,
        out.employee_id,
        "Tax proof rejected",
        &msg,
    )
    .await;
    Ok(out)
}

/// Sums `actual_amount` for **APPROVED** lines and writes the result to
/// `tax_computation.total_deductions` for the same employee / config / fiscal year.
/// Year-end and payroll logic should use that column (not unapproved `actual_amount` values).
pub async fn recompute_total_deductions_from_approved_proofs<C>(
    db: &C,
    tenant_id: Uuid,
    employee_id: Uuid,
    tax_config_version_id: Uuid,
    fiscal_year: i32,
) -> KabiPayResult<()>
where
    C: ConnectionTrait,
{
    let lines = tax_proof_line::Entity::find()
        .filter(tax_proof_line::Column::TenantId.eq(tenant_id))
        .filter(tax_proof_line::Column::EmployeeId.eq(employee_id))
        .filter(tax_proof_line::Column::TaxConfigVersionId.eq(tax_config_version_id))
        .filter(tax_proof_line::Column::FiscalYear.eq(fiscal_year))
        .filter(tax_proof_line::Column::Status.eq(PROOF_APPROVED))
        .all(db)
        .await?;
    let sum: Decimal = lines
        .iter()
        .fold(Decimal::ZERO, |acc, l| acc + l.actual_amount);
    let now = Utc::now();
    let existing = tax_computation::Entity::find()
        .filter(tax_computation::Column::TenantId.eq(tenant_id))
        .filter(tax_computation::Column::EmployeeId.eq(employee_id))
        .filter(tax_computation::Column::TaxConfigVersionId.eq(tax_config_version_id))
        .filter(tax_computation::Column::FiscalYear.eq(fiscal_year))
        .one(db)
        .await?;
    if let Some(row) = existing {
        let mut am: tax_computation::ActiveModel = row.into();
        am.total_deductions = Set(Some(sum));
        am.computed_at = Set(now);
        am.update(db).await?;
    } else {
        let id = Uuid::new_v4();
        let am = tax_computation::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            employee_id: Set(employee_id),
            tax_config_version_id: Set(tax_config_version_id),
            fiscal_year: Set(fiscal_year),
            tax_regime_chosen: Set(None),
            gross_income: Set(None),
            total_deductions: Set(Some(sum)),
            taxable_income: Set(None),
            final_tax: Set(None),
            tds_per_month: Set(None),
            computed_at: Set(now),
        };
        am.insert(db).await?;
    }
    Ok(())
}

/// Admin-editable catalogue matching **`tax_proof_line.section_code`** labels (India IT sections, etc.).
pub async fn list_tax_section_definitions(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    active_only: bool,
    limit: u64,
) -> KabiPayResult<Vec<tax_section_definition::Model>> {
    let limit = limit.clamp(1, 200);
    let mut q = tax_section_definition::Entity::find()
        .filter(tax_section_definition::Column::TenantId.eq(tenant_id));
    if active_only {
        q = q.filter(tax_section_definition::Column::IsActive.eq(true));
    }
    q.order_by_asc(tax_section_definition::Column::DisplayOrder)
        .order_by_asc(tax_section_definition::Column::SectionCode)
        .limit(limit)
        .all(db)
        .await
        .map_err(KabiPayError::from)
}

/// Insert or update a section row by `(tenant_id, section_code)`.
pub async fn upsert_tax_section_definition(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    section_code: String,
    section_label: String,
    regime_scope: Option<String>,
    country_code: String,
    display_order: i32,
    is_active: bool,
    max_deduction_amount: Option<Decimal>,
) -> KabiPayResult<tax_section_definition::Model> {
    let sc = section_code.trim().to_uppercase();
    if sc.is_empty() {
        return Err(KabiPayError::Validation(
            "sectionCode must not be empty".into(),
        ));
    }
    let label = section_label.trim().to_string();
    if label.is_empty() {
        return Err(KabiPayError::Validation(
            "sectionLabel must not be empty".into(),
        ));
    }
    let cc = country_code.trim().to_uppercase();
    if cc.is_empty() {
        return Err(KabiPayError::Validation(
            "countryCode must not be empty".into(),
        ));
    }
    let regime = regime_scope.and_then(|r| {
        let t = r.trim().to_uppercase();
        if t.is_empty() {
            None
        } else {
            Some(t)
        }
    });
    let now = Utc::now();

    let existing = tax_section_definition::Entity::find()
        .filter(tax_section_definition::Column::TenantId.eq(tenant_id))
        .filter(tax_section_definition::Column::SectionCode.eq(&sc))
        .one(db)
        .await
        .map_err(KabiPayError::from)?;

    if let Some(row) = existing {
        let id = row.id;
        let mut am: tax_section_definition::ActiveModel = row.into();
        am.section_label = Set(label);
        am.regime_scope = Set(regime);
        am.country_code = Set(cc);
        am.display_order = Set(display_order);
        am.is_active = Set(is_active);
        am.max_deduction_amount = Set(max_deduction_amount);
        am.updated_at = Set(now);
        am.update(db).await.map_err(KabiPayError::from)?;
        tax_section_definition::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("updated tax_section_definition".into()))
    } else {
        let id = Uuid::new_v4();
        tax_section_definition::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            section_code: Set(sc),
            section_label: Set(label),
            regime_scope: Set(regime),
            country_code: Set(cc),
            display_order: Set(display_order),
            is_active: Set(is_active),
            max_deduction_amount: Set(max_deduction_amount),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(KabiPayError::from)?;
        tax_section_definition::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("inserted tax_section_definition".into()))
    }
}

pub async fn upsert_tax_configuration_version(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id_opt: Option<Uuid>,
    fiscal_year: i32,
    regime: Option<String>,
    country_code: String,
    is_active: bool,
) -> KabiPayResult<tax_configuration_version::Model> {
    let cc = country_code.trim().to_uppercase();
    if cc.is_empty() {
        return Err(KabiPayError::Validation("countryCode must not be empty".into()));
    }
    let reg = regime.and_then(|r| {
        let t = r.trim();
        if t.is_empty() {
            None
        } else {
            Some(t.to_string())
        }
    });
    let now = Utc::now();
    if let Some(id) = id_opt {
        let row = tax_configuration_version::Entity::find()
            .filter(tax_configuration_version::Column::Id.eq(id))
            .filter(tax_configuration_version::Column::TenantId.eq(tenant_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "tax_configuration_version",
                id: id.to_string(),
            })?;
        let mut am: tax_configuration_version::ActiveModel = row.into();
        am.fiscal_year = Set(fiscal_year);
        am.regime = Set(reg);
        am.country_code = Set(cc);
        am.is_active = Set(is_active);
        am.updated_at = Set(now);
        am.update(db).await.map_err(KabiPayError::from)?;
        tax_configuration_version::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("updated tax_configuration_version missing".into()))
    } else {
        let id = Uuid::new_v4();
        tax_configuration_version::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            fiscal_year: Set(fiscal_year),
            regime: Set(reg),
            country_code: Set(cc),
            is_active: Set(is_active),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(KabiPayError::from)?;
        tax_configuration_version::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("inserted tax_configuration_version missing".into()))
    }
}

pub async fn upsert_tax_slab(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    id_opt: Option<Uuid>,
    tax_config_version_id: Uuid,
    income_from: Decimal,
    income_to: Option<Decimal>,
    tax_rate: Option<Decimal>,
    surcharge_rate: Option<Decimal>,
    cess_rate: Option<Decimal>,
) -> KabiPayResult<tax_slab::Model> {
    let _config = tax_configuration_version::Entity::find()
        .filter(tax_configuration_version::Column::Id.eq(tax_config_version_id))
        .filter(tax_configuration_version::Column::TenantId.eq(tenant_id))
        .one(db)
        .await
        .map_err(KabiPayError::from)?
        .ok_or_else(|| KabiPayError::NotFound {
            entity: "tax_configuration_version",
            id: tax_config_version_id.to_string(),
        })?;
    let now = Utc::now();
    if let Some(id) = id_opt {
        let row = tax_slab::Entity::find()
            .filter(tax_slab::Column::Id.eq(id))
            .filter(tax_slab::Column::TenantId.eq(tenant_id))
            .filter(tax_slab::Column::TaxConfigVersionId.eq(tax_config_version_id))
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::NotFound {
                entity: "tax_slab",
                id: id.to_string(),
            })?;
        let mut am: tax_slab::ActiveModel = row.into();
        am.income_from = Set(income_from);
        am.income_to = Set(income_to);
        am.tax_rate = Set(tax_rate);
        am.surcharge_rate = Set(surcharge_rate);
        am.cess_rate = Set(cess_rate);
        am.updated_at = Set(now);
        am.update(db).await.map_err(KabiPayError::from)?;
        tax_slab::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("updated tax_slab missing".into()))
    } else {
        let id = Uuid::new_v4();
        tax_slab::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant_id),
            tax_config_version_id: Set(tax_config_version_id),
            income_from: Set(income_from),
            income_to: Set(income_to),
            tax_rate: Set(tax_rate),
            surcharge_rate: Set(surcharge_rate),
            cess_rate: Set(cess_rate),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await
        .map_err(KabiPayError::from)?;
        tax_slab::Entity::find_by_id(id)
            .one(db)
            .await
            .map_err(KabiPayError::from)?
            .ok_or_else(|| KabiPayError::Internal("inserted tax_slab missing".into()))
    }
}

async fn tax_proof_notify_employee(
    db: &DatabaseConnection,
    tenant_id: Uuid,
    employee_id: Uuid,
    title: &str,
    message: &str,
) {
    let user_id: Option<Uuid> = match employee::Entity::find_by_id(employee_id)
        .filter(employee::Column::TenantId.eq(tenant_id))
        .filter(employee::Column::IsDeleted.eq(false))
        .one(db)
        .await
    {
        Ok(Some(emp)) => emp.user_id,
        _ => None,
    };
    let Some(user_id) = user_id else {
        return;
    };
    let now = Utc::now();
    let am = notification::ActiveModel {
        id: Set(Uuid::new_v4()),
        tenant_id: Set(tenant_id),
        user_id: Set(user_id),
        r#type: Set(Some("TAX".into())),
        title: Set(Some(title.into())),
        message: Set(Some(message.into())),
        action_url: Set(None),
        is_read: Set(false),
        read_at: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    };
    if let Err(e) = am.insert(db).await {
        tracing::warn!(error = %e, "insert notification (tax proof) failed");
    }
}

#[cfg(test)]
mod decision_transaction_tests {
    use super::*;
    use kabipay_common::context::ScopeType;
    use sea_orm::entity::prelude::async_trait;
    use sea_orm::{
        Database, DbBackend, DbErr, ProxyDatabaseTrait, ProxyExecResult, ProxyRow, QueryTrait,
    };
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::{Arc, Mutex};

    #[derive(Debug)]
    struct ScriptedProxy {
        query_results: Mutex<VecDeque<Result<Vec<ProxyRow>, DbErr>>>,
        events: Arc<Mutex<Vec<String>>>,
    }

    #[async_trait::async_trait]
    impl ProxyDatabaseTrait for ScriptedProxy {
        async fn query(&self, statement: sea_orm::Statement) -> Result<Vec<ProxyRow>, DbErr> {
            self.events
                .lock()
                .expect("event recorder")
                .push(format!("QUERY {statement}"));
            self.query_results
                .lock()
                .expect("query script")
                .pop_front()
                .unwrap_or_else(|| Ok(Vec::new()))
        }

        async fn execute(
            &self,
            statement: sea_orm::Statement,
        ) -> Result<ProxyExecResult, DbErr> {
            self.events
                .lock()
                .expect("event recorder")
                .push(format!("EXECUTE {statement}"));
            Ok(ProxyExecResult {
                last_insert_id: 0,
                rows_affected: 1,
            })
        }

        async fn begin(&self) {
            self.events.lock().expect("event recorder").push("BEGIN".into());
        }

        async fn commit(&self) {
            self.events.lock().expect("event recorder").push("COMMIT".into());
        }

        async fn rollback(&self) {
            self.events
                .lock()
                .expect("event recorder")
                .push("ROLLBACK".into());
        }
    }

    async fn scripted_connection(
        query_results: Vec<Result<Vec<ProxyRow>, DbErr>>,
    ) -> (DatabaseConnection, Arc<Mutex<Vec<String>>>) {
        let events = Arc::new(Mutex::new(Vec::new()));
        let db = Database::connect_proxy(
            DbBackend::Postgres,
            Arc::new(Box::new(ScriptedProxy {
                query_results: Mutex::new(query_results.into()),
                events: Arc::clone(&events),
            })),
        )
        .await
        .expect("PostgreSQL proxy connection");
        (db, events)
    }

    fn proof_model(status: &str) -> tax_proof_line::Model {
        let now = Utc::now();
        tax_proof_line::Model {
            id: Uuid::new_v4(),
            tenant_id: Uuid::new_v4(),
            employee_id: Uuid::new_v4(),
            tax_config_version_id: Uuid::new_v4(),
            fiscal_year: 2026,
            section_code: "80C".into(),
            declared_amount: Decimal::new(1000, 0),
            actual_amount: Decimal::new(900, 0),
            file_storage_id: Some(Uuid::new_v4()),
            status: status.into(),
            rejection_reason: None,
            approved_by: None,
            submitted_at: now,
            created_at: now,
            updated_at: now,
        }
    }

    fn proof_row(model: &tax_proof_line::Model) -> ProxyRow {
        ProxyRow::new(BTreeMap::from([
            ("id".into(), model.id.into()),
            ("tenant_id".into(), model.tenant_id.into()),
            ("employee_id".into(), model.employee_id.into()),
            (
                "tax_config_version_id".into(),
                model.tax_config_version_id.into(),
            ),
            ("fiscal_year".into(), model.fiscal_year.into()),
            ("section_code".into(), model.section_code.clone().into()),
            ("declared_amount".into(), model.declared_amount.into()),
            ("actual_amount".into(), model.actual_amount.into()),
            ("file_storage_id".into(), model.file_storage_id.into()),
            ("status".into(), model.status.clone().into()),
            (
                "rejection_reason".into(),
                model.rejection_reason.clone().into(),
            ),
            ("approved_by".into(), model.approved_by.into()),
            ("submitted_at".into(), model.submitted_at.into()),
            ("created_at".into(), model.created_at.into()),
            ("updated_at".into(), model.updated_at.into()),
        ]))
    }

    #[test]
    fn concurrent_decisions_lock_the_same_tenant_proof_row_for_update() {
        let model = proof_model(PROOF_PENDING);
        let sql = tax_proof_for_decision_query(model.tenant_id, model.id)
            .build(DbBackend::Postgres)
            .to_string();
        assert!(sql.contains(&model.tenant_id.to_string()));
        assert!(sql.contains(&model.id.to_string()));
        assert!(sql.ends_with("FOR UPDATE"));
    }

    #[test]
    fn duplicate_and_stale_decisions_share_a_stable_conflict() {
        assert!(require_pending_tax_proof_status(PROOF_PENDING).is_ok());
        for stale_status in [PROOF_APPROVED, PROOF_REJECTED] {
            let error = require_pending_tax_proof_status(stale_status)
                .expect_err("a serialized second decision must be rejected");
            assert_eq!(error.code(), "CONFLICT");
        }
    }

    #[tokio::test]
    async fn recompute_failure_rolls_back_the_proof_update() {
        let pending = proof_model(PROOF_PENDING);
        let mut rejected = pending.clone();
        rejected.status = PROOF_REJECTED.into();
        rejected.rejection_reason = Some("invalid".into());
        let (db, events) = scripted_connection(vec![
            Ok(vec![proof_row(&pending)]),
            Ok(vec![proof_row(&rejected)]),
            Err(DbErr::Custom("forced recompute failure".into())),
        ])
        .await;

        let error = reject_tax_proof_line(
            &db,
            pending.tenant_id,
            pending.id,
            ScopeType::All,
            Uuid::new_v4(),
            Some("invalid".into()),
        )
        .await
        .expect_err("recompute failure must abort the entire decision");
        assert_eq!(error.code(), "DATABASE_ERROR");

        let events = events.lock().expect("event recorder");
        assert!(events.iter().any(|event| event == "BEGIN"));
        assert!(events.iter().any(|event| event == "ROLLBACK"));
        assert!(!events.iter().any(|event| event == "COMMIT"));
    }

    #[tokio::test]
    async fn stale_decision_rolls_back_without_updating_or_recomputing() {
        let stale = proof_model(PROOF_APPROVED);
        let (db, events) = scripted_connection(vec![Ok(vec![proof_row(&stale)])]).await;

        let error = reject_tax_proof_line(
            &db,
            stale.tenant_id,
            stale.id,
            ScopeType::All,
            Uuid::new_v4(),
            None,
        )
        .await
        .expect_err("stale proof decision must fail");
        assert_eq!(error.code(), "CONFLICT");

        let events = events.lock().expect("event recorder");
        assert!(events.iter().any(|event| event == "ROLLBACK"));
        assert!(!events.iter().any(|event| event.starts_with("EXECUTE")));
        assert!(!events.iter().any(|event| event == "COMMIT"));
    }
}
