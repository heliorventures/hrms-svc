//! Versioned structures and effective assignments, using the shared salary validator.
use anyhow::{bail, Result};
use chrono::{NaiveDate, Utc};
use kabipay_db_entities::tenant::d0012_payroll::{
    salary_component, salary_structure, salary_structure_component,
};
use kabipay_payroll::services::salary_rules::{validate_recurring_salary, RecurringSalary};
use sea_orm::{
    ActiveModelTrait, ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Set,
    Statement,
};
use sha2::{Digest, Sha256};
use uuid::Uuid;

pub async fn ensure_component<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    code: &str,
    kind: &str,
) -> Result<Uuid> {
    let existing = salary_component::Entity::find()
        .filter(salary_component::Column::TenantId.eq(tenant))
        .filter(salary_component::Column::Code.eq(code))
        .all(db)
        .await?;
    if existing.len() > 1 {
        bail!("COMPONENT_IDENTITY_AMBIGUOUS");
    }
    if let Some(component) = existing.first() {
        if component.r#type != kind || !component.is_active {
            bail!("COMPONENT_CONFIGURATION_CONFLICT");
        }
        return Ok(component.id);
    }
    let now = Utc::now();
    let id = Uuid::new_v4();
    salary_component::ActiveModel {
        id: Set(id),
        tenant_id: Set(tenant),
        name: Set(code.replace('_', " ")),
        code: Set(code.into()),
        r#type: Set(kind.into()),
        is_taxable: Set(false),
        is_fixed: Set(false),
        is_active: Set(true),
        formula_expression: Set(None),
        created_at: Set(now),
        updated_at: Set(now),
    }
    .insert(db)
    .await?;
    if kind == "EMPLOYER_CONTRIBUTION" {
        kabipay_payroll::services::component_display::save(db, tenant, id, false).await?;
    }
    Ok(id)
}

pub async fn salary<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    actor: Uuid,
    employee: Uuid,
    effective: NaiveDate,
    value: &serde_json::Value,
) -> Result<&'static str> {
    let input: RecurringSalary = serde_json::from_value(value.clone())?;
    let validated = validate_recurring_salary(&input)?;
    let fingerprint = hex::encode(Sha256::digest(serde_json::to_vec(value)?));
    let name = format!("Imported salary {}", &fingerprint[..24]);
    let existing = salary_structure::Entity::find()
        .filter(salary_structure::Column::TenantId.eq(tenant))
        .filter(salary_structure::Column::Name.eq(&name))
        .all(db)
        .await?;
    if existing.len() > 1 {
        bail!("SALARY_STRUCTURE_AMBIGUOUS");
    }
    let now = Utc::now();
    let structure = if let Some(existing) = existing.first() {
        let rows=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT c.code,c.type,c.is_active,s.calculation_basis,s.calculation_value,s.amount,s.percentage_of_basic FROM salary_structure_component s JOIN salary_component c ON c.tenant_id=s.tenant_id AND c.id=s.salary_component_id WHERE s.tenant_id=$1 AND s.salary_structure_id=$2",
            [tenant.into(),existing.id.into()])).await?;
        let mut seen = std::collections::HashSet::new();
        if rows.len() != validated.components.len() {
            bail!("IMPORTED_SALARY_STRUCTURE_CHANGED_REQUIRES_REVIEW");
        }
        for row in rows {
            let code = row.try_get::<String>("", "code")?;
            if !seen.insert(code.clone())
                || row.try_get::<String>("", "type")? != "EARNING"
                || !row.try_get::<bool>("", "is_active")?
                || row.try_get::<String>("", "calculation_basis")? != "FIXED_MONTHLY"
                || row.try_get::<Option<rust_decimal::Decimal>>("", "calculation_value")?
                    != validated.components.get(&code).copied()
                || row
                    .try_get::<Option<rust_decimal::Decimal>>("", "amount")?
                    .is_some()
                || row
                    .try_get::<Option<rust_decimal::Decimal>>("", "percentage_of_basic")?
                    .is_some()
            {
                bail!("IMPORTED_SALARY_STRUCTURE_CHANGED_REQUIRES_REVIEW");
            }
        }
        existing.id
    } else {
        let id = Uuid::new_v4();
        salary_structure::ActiveModel {
            id: Set(id),
            tenant_id: Set(tenant),
            name: Set(name),
            description: Set(Some(
                "Gross-based recurring salary; period incentives and advances are separate".into(),
            )),
            created_at: Set(now),
            updated_at: Set(now),
        }
        .insert(db)
        .await?;
        for (order, (code, amount)) in validated.components.iter().enumerate() {
            let component = ensure_component(db, tenant, code, "EARNING").await?;
            salary_structure_component::ActiveModel {
                id: Set(Uuid::new_v4()),
                tenant_id: Set(tenant),
                salary_structure_id: Set(id),
                salary_component_id: Set(component),
                amount: Set(None),
                percentage_of_basic: Set(None),
                calculation_basis: Set("FIXED_MONTHLY".into()),
                calculation_value: Set(Some(*amount)),
                display_order: Set(order as i32),
                created_at: Set(now),
                updated_at: Set(now),
            }
            .insert(db)
            .await?;
        }
        id
    };
    for (code, kind) in [
        ("PF", "DEDUCTION"),
        ("ESI", "DEDUCTION"),
        ("PT", "DEDUCTION"),
        ("TDS", "DEDUCTION"),
        ("PF_EMPLOYER", "EMPLOYER_CONTRIBUTION"),
        ("ESI_EMPLOYER", "EMPLOYER_CONTRIBUTION"),
        ("INCENTIVE", "EARNING"),
    ] {
        ensure_component(db, tenant, code, kind).await?;
    }
    let assignments=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT id,salary_structure_id,annual_gross,annual_employer_pf FROM employee_salary_structure WHERE tenant_id=$1 AND employee_id=$2 AND effective_from=$3",
        [tenant.into(),employee.into(),effective.into()])).await?;
    if assignments.len() > 1 {
        bail!("SALARY_ASSIGNMENT_AMBIGUOUS");
    }
    let ctc_storage = validated.annual_ctc.unwrap_or(validated.annual_gross);
    let outcome = if let Some(existing) = assignments.first() {
        if existing.try_get::<Uuid>("", "salary_structure_id")? == structure
            && existing.try_get::<Option<rust_decimal::Decimal>>("", "annual_gross")?
                == Some(validated.annual_gross)
            && existing.try_get::<Option<rust_decimal::Decimal>>("", "annual_employer_pf")?
                == validated.annual_employer_pf
        {
            "UNCHANGED"
        } else {
            // An existing financial assignment can be changed only before any affected slip exists.
            require_unused_period(db, tenant, employee, effective).await?;
            db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
                "UPDATE employee_salary_structure SET salary_structure_id=$3,ctc=$4,annual_gross=$5,annual_employer_pf=$6,updated_at=NOW() WHERE tenant_id=$1 AND id=$2",
                [tenant.into(),existing.try_get::<Uuid>("","id")?.into(),structure.into(),ctc_storage.into(),validated.annual_gross.into(),validated.annual_employer_pf.into()])).await?;
            "UPDATED"
        }
    } else {
        require_unused_period(db, tenant, employee, effective).await?;
        let overlaps=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
            "SELECT id FROM employee_salary_structure WHERE tenant_id=$1 AND employee_id=$2 AND effective_from>$3",
            [tenant.into(),employee.into(),effective.into()])).await?;
        if !overlaps.is_empty() {
            bail!("LATER_SALARY_ASSIGNMENT_REQUIRES_REVIEW");
        }
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "UPDATE employee_salary_structure SET effective_to=$3::date-1,updated_at=NOW() WHERE tenant_id=$1 AND employee_id=$2 AND effective_from<$3 AND (effective_to IS NULL OR effective_to>=$3)",
            [tenant.into(),employee.into(),effective.into()])).await?;
        db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO employee_salary_structure(id,tenant_id,employee_id,salary_structure_id,ctc,annual_gross,annual_employer_pf,effective_from) VALUES($1,$2,$3,$4,$5,$6,$7,$8)",
            [Uuid::new_v4().into(),tenant.into(),employee.into(),structure.into(),ctc_storage.into(),validated.annual_gross.into(),validated.annual_employer_pf.into(),effective.into()])).await?;
        "CREATED"
    };
    db.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "INSERT INTO employee_payroll_rule(tenant_id,employee_id,effective_from,rules,updated_by) VALUES($1,$2,$3,$4,$5) ON CONFLICT(tenant_id,employee_id,effective_from) DO UPDATE SET rules=EXCLUDED.rules,updated_by=EXCLUDED.updated_by,updated_at=NOW() WHERE employee_payroll_rule.rules IS DISTINCT FROM EXCLUDED.rules",
        [tenant.into(),employee.into(),effective.into(),value.clone().into(),actor.into()])).await?;
    Ok(outcome)
}

async fn require_unused_period<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    employee: Uuid,
    effective: NaiveDate,
) -> Result<()> {
    let slips=db.query_all(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT p.id FROM payslip p JOIN payroll_cycle c ON c.id=p.payroll_cycle_id AND c.tenant_id=p.tenant_id WHERE p.tenant_id=$1 AND p.employee_id=$2 AND (make_date(c.year,c.month,1)+INTERVAL '1 month - 1 day')::date>=$3 LIMIT 1",
        [tenant.into(),employee.into(),effective.into()])).await?;
    if !slips.is_empty() {
        bail!("SALARY_ALREADY_USED_REQUIRES_NEW_EFFECTIVE_DATE");
    }
    Ok(())
}
