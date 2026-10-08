//! Resolve proofs for the chosen annual regime; never reuse another regime's cache.
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use uuid::Uuid;

pub async fn old_regime_total<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    year: i32,
) -> KabiPayResult<Decimal> {
    // A later pending/rejected resubmission replaces an earlier approved version
    // of the same section. Do not sum duplicate proofs across configuration versions.
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COALESCE(SUM(CASE WHEN status='APPROVED' THEN actual_amount ELSE 0 END),0) AS total,COUNT(*)-COUNT(DISTINCT section_code) AS ambiguous FROM (SELECT p.section_code,p.status,p.actual_amount,DENSE_RANK() OVER(PARTITION BY p.section_code ORDER BY p.submitted_at DESC) AS submission_rank FROM tax_proof_line p JOIN tax_configuration_version v ON v.tenant_id=p.tenant_id AND v.id=p.tax_config_version_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND p.fiscal_year=$3 AND v.fiscal_year=$3 AND UPPER(v.regime) IN ('OLD','OLD_REGIME')) ranked WHERE submission_rank=1",
        [tenant.into(),employee.into(),year.into()])).await?
        .ok_or_else(||KabiPayError::Internal("approved deduction total unavailable".into()))?;
    // Approval changes updated_at, but must never change which submission supersedes
    // another. If submission chronology itself is ambiguous, require resubmission.
    if row.try_get::<i64>("", "ambiguous")? != 0 {
        return Err(KabiPayError::Validation("Tax proof submissions have the same submission time for a section; HR must request a clear latest submission before calculating deductions".into()));
    }
    Ok(row.try_get("", "total")?)
}
