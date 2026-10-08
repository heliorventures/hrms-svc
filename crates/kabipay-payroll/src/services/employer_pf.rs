//! Effective employer costs are separate from employee earnings and deductions.
use super::{payroll_rules::{amount, money}, salary_rules::EmployerPfRule};
use chrono::NaiveDate;
use kabipay_common::{KabiPayError, KabiPayResult};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DbBackend, Statement};
use std::collections::{BTreeMap, BTreeSet};
use uuid::Uuid;

pub async fn effective<C: ConnectionTrait>(db: &C, tenant: Uuid, employee: Uuid, date: NaiveDate) -> KabiPayResult<Option<EmployerPfRule>> {
    let row = db.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT rules->'employer_pf_rule' AS value FROM employee_payroll_rule WHERE tenant_id=$1 AND employee_id=$2 AND effective_from<=$3 AND rules ? 'employer_pf_rule' ORDER BY effective_from DESC LIMIT 1",
        [tenant.into(), employee.into(), date.into()])).await?;
    row.map(|row| serde_json::from_value(row.try_get("", "value")?)
        .map_err(|_| KabiPayError::Validation("stored employer PF rule requires review".into())))
        .transpose().map(Option::flatten)
}

pub fn monthly(rule: &EmployerPfRule, components: &BTreeMap<String, Decimal>) -> KabiPayResult<Decimal> {
    if rule.rounding != "HALF_UP_2DP" {
        return Err(KabiPayError::Validation("unsupported employer PF rounding".into()));
    }
    let rate = amount(Some(&rule.rate), "employer PF rate")?;
    if let Some(fixed) = &rule.fixed_monthly_amount {
        if !rule.basis_components.is_empty() || !rate.is_zero() || rule.ceiling.is_some() || rule.origin != "REVIEWED_CONFIGURATION" {
            return Err(KabiPayError::Validation("fixed employer PF requires a reviewed amount without a simultaneous wage formula".into()));
        }
        return amount(Some(fixed), "fixed employer PF").map(money);
    }
    if rule.basis_components.is_empty() || rate > Decimal::ONE {
        return Err(KabiPayError::Validation("invalid employer PF rate or basis".into()));
    }
    let mut seen = BTreeSet::new();
    let mut base = Decimal::ZERO;
    for code in &rule.basis_components {
        if !seen.insert(code) { return Err(KabiPayError::Validation("duplicate employer PF basis component".into())); }
        let value = components.get(code).ok_or_else(|| KabiPayError::Validation("employer PF basis component is missing".into()))?;
        kabipay_tax::domain::validate_amount(*value)?;
        base += value;
    }
    if let Some(ceiling) = &rule.ceiling { base = base.min(amount(Some(ceiling), "PF ceiling")?); }
    Ok(money(base * rate))
}
