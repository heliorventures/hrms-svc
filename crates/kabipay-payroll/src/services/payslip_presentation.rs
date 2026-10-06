//! One company catalog controls browser, print and PDF presentation without changing pay.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::{
    d0012_payroll::{payslip, payslip_component},
    d0090_payroll_period_configuration::payslip_statement,
};
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use uuid::Uuid;

#[derive(Clone, Debug, Serialize, Deserialize, async_graphql::SimpleObject)]
pub struct PayslipDisplayLine {
    pub id: String,
    pub code: String,
    pub name: String,
    pub component_type: String,
    pub amount: String,
}
#[derive(Clone, Debug, async_graphql::SimpleObject)]
pub struct PayslipPresentation {
    pub template: String,
    pub lines: Vec<PayslipDisplayLine>,
    pub statement: Option<async_graphql::Json<serde_json::Value>>,
}
struct Component {
    code: String,
    name: String,
    kind: String,
    visible: bool,
}
pub async fn load<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    slip: &payslip::Model,
    lines: &[payslip_component::Model],
) -> KabiPayResult<PayslipPresentation> {
    let template = super::payslip_template::load(db, tenant).await?;
    let rows = db
        .query_all(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT id,code,name,type,show_on_payslip FROM salary_component WHERE tenant_id=$1",
            [tenant.into()],
        ))
        .await?;
    let mut catalog: HashMap<Uuid, Component> = HashMap::new();
    for row in rows {
        catalog.insert(
            row.try_get("", "id")?,
            Component {
                code: row.try_get("", "code")?,
                name: row.try_get("", "name")?,
                kind: row.try_get("", "type")?,
                visible: row.try_get("", "show_on_payslip")?,
            },
        );
    }
    let mut display = Vec::new();
    let mut seen = HashSet::new();
    for line in lines {
        let component = catalog.get(&line.salary_component_id).ok_or_else(|| {
            KabiPayError::Validation("payslip component catalog is unresolved".into())
        })?;
        seen.insert(component.code.clone());
        if component.visible {
            display.push(PayslipDisplayLine {
                id: line.id.to_string(),
                code: component.code.clone(),
                name: component.name.clone(),
                component_type: component.kind.clone(),
                amount: line.amount.to_string(),
            });
        }
    }
    let statement = payslip_statement::Entity::find()
        .filter(payslip_statement::Column::TenantId.eq(tenant))
        .filter(payslip_statement::Column::PayslipId.eq(slip.id))
        .one(db)
        .await?;
    let mut statement = statement.map(|row| row.statement);
    for (code, value, kind) in [
        ("PF", slip.pf_employee, "DEDUCTION"),
        ("ESI", slip.esi_employee, "DEDUCTION"),
        ("PT", slip.professional_tax, "DEDUCTION"),
        ("TDS", slip.tds_amount, "DEDUCTION"),
        ("PF_EMPLOYER", slip.pf_employer, "EMPLOYER_CONTRIBUTION"),
        ("ESI_EMPLOYER", slip.esi_employer, "EMPLOYER_CONTRIBUTION"),
    ] {
        if let Some(value) = value {
            add_line(
                &mut display,
                &mut seen,
                &catalog,
                code,
                &value.to_string(),
                kind,
            );
        }
    }
    if let Some(value) = &mut statement {
        if let Some(incentive) = value["incentive"].as_str() {
            add_line(
                &mut display,
                &mut seen,
                &catalog,
                "INCENTIVE",
                incentive,
                "EARNING",
            );
        }
        if let Some(items) = value["additional_deductions"].as_array() {
            for item in items {
                if let (Some(code), Some(amount)) = (item["code"].as_str(), item["amount"].as_str())
                {
                    add_line(&mut display, &mut seen, &catalog, code, amount, "DEDUCTION");
                }
            }
        }
        // Calculations retain employer costs; the employee-facing statement only needs earnings and settlement.
        if let Some(object) = value.as_object_mut() {
            for key in [
                "employer",
                "components",
                "statutory",
                "additional_deductions",
            ] {
                object.remove(key);
            }
        }
    }
    Ok(PayslipPresentation {
        template,
        lines: display,
        statement: statement.map(async_graphql::Json),
    })
}
fn add_line(
    lines: &mut Vec<PayslipDisplayLine>,
    seen: &mut HashSet<String>,
    catalog: &HashMap<Uuid, Component>,
    code: &str,
    amount: &str,
    kind: &str,
) {
    if seen.contains(code) {
        return;
    }
    let component = catalog
        .values()
        .find(|item| item.code == code && item.kind == kind);
    let visible = component
        .map(|item| item.visible)
        .unwrap_or(kind != "EMPLOYER_CONTRIBUTION");
    if visible {
        lines.push(PayslipDisplayLine {
            id: format!("summary-{code}"),
            code: code.into(),
            name: component
                .map(|item| item.name.clone())
                .unwrap_or_else(|| code.replace('_', " ")),
            component_type: kind.into(),
            amount: amount.into(),
        });
    }
    seen.insert(code.into());
}
