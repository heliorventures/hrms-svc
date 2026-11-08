//! Freeze the explicit input set before any payroll write or advisory/row lock.
//! Table locks also cover legacy writers that do not use payroll advisory locks.
use kabipay_common::{KabiPayError, KabiPayResult};
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use sha2::{Digest, Sha256};
use uuid::Uuid;

const INPUT_TABLES: &[&str] = &[
    "company_payroll_rule",
    "employee",
    "employee_location_assignment",
    "employee_payroll_rule",
    "employee_salary_component_override",
    "employee_salary_structure",
    "employee_tax_declaration",
    "employee_tax_history",
    "employee_tax_settings",
    "holiday",
    "holiday_calendar",
    "leave_request",
    "leave_working_date_snapshot",
    "leave_type",
    "payroll_arrear",
    "payroll_cycle",
    "payroll_period_input",
    "payroll_unpaid_leave_allocation",
    "payroll_unpaid_leave_policy",
    "payslip",
    "payslip_component",
    "payslip_loan_snapshot",
    "payslip_statement",
    "salary_component",
    "salary_structure",
    "salary_structure_component",
    "tax_proof_line",
    "tax_configuration_version",
    "weekly_off_policy_version",
    "working_calendar_profile",
];

pub async fn lock_inputs<C: ConnectionTrait>(db: &C) -> KabiPayResult<()> {
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        "SET LOCAL lock_timeout='15s'",
    ))
    .await?;
    // One sorted lock statement; errors roll back rather than continuing on a partial snapshot.
    let mut tables = INPUT_TABLES.to_vec();
    tables.push("payroll_draft_calculation");
    tables.sort_unstable();
    db.execute(Statement::from_string(
        DbBackend::Postgres,
        format!(
            "LOCK TABLE {} IN SHARE ROW EXCLUSIVE MODE",
            tables.join(",")
        ),
    ))
    .await?;
    Ok(())
}

pub async fn fingerprint<C: ConnectionTrait>(db: &C, tenant: Uuid) -> KabiPayResult<String> {
    let mut digest = Sha256::new();
    digest.update(b"payroll-draft-v2-reviewed-remuneration");
    for table in INPUT_TABLES {
        // Names are compile-time identifiers, never user input. Include row revisions and
        // effective dates, so an equal-amount HR edit still invalidates the reviewed draft.
        let ownership = if *table == "holiday" {
            "calendar_id IN (SELECT id FROM holiday_calendar WHERE tenant_id=$1)"
        } else {
            "tenant_id=$1"
        };
        let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
            format!("SELECT COALESCE(jsonb_agg(to_jsonb(t) ORDER BY to_jsonb(t)::text),'[]'::jsonb)::text AS value FROM {table} t WHERE {ownership}"),
            [tenant.into()])).await?.ok_or_else(|| KabiPayError::Internal("payroll fingerprint unavailable".into()))?;
        digest.update(table.as_bytes());
        digest.update(row.try_get::<String>("", "value")?.as_bytes());
    }
    Ok(hex::encode(digest.finalize()))
}
