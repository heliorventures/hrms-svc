//! Allow-listed payslip details, resolved only after the caller is authorized for the payslip.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::{payroll_compliance_setting, payslip};
use sea_orm::{ColumnTrait, ConnectionTrait, DbBackend, EntityTrait, QueryFilter, Statement};
use serde_json::Value;
use uuid::Uuid;

const FIELDS: &[(&str, &str)] = &[
    ("EMPLOYEE_NAME", "Employee"),
    ("EMPLOYEE_CODE", "Employee code"),
    ("DEPARTMENT", "Department"),
    ("DESIGNATION", "Designation"),
    ("JOINING_DATE", "Joining date"),
    ("GENDER", "Gender"),
    ("MARITAL_STATUS", "Marital status"),
    ("UAN", "UAN"),
    ("ESIC", "ESIC"),
    ("PAYSLIP_STATUS", "Status"),
    ("GENERATED_DATE", "Generated date"),
];

pub fn defaults() -> Vec<String> {
    ["EMPLOYEE_NAME", "EMPLOYEE_CODE", "UAN", "ESIC"]
        .into_iter()
        .map(str::to_owned)
        .collect()
}

pub fn validate(fields: &[String]) -> KabiPayResult<()> {
    let mut seen = std::collections::HashSet::new();
    for field in fields {
        if !FIELDS.iter().any(|(id, _)| *id == field.as_str()) || !seen.insert(field) {
            return Err(KabiPayError::Validation(
                "unsupported or duplicate payslip employee field".into(),
            ));
        }
    }
    Ok(())
}

pub fn decode(value: Value) -> KabiPayResult<Vec<String>> {
    let fields: Vec<String> = serde_json::from_value(value)
        .map_err(|_| KabiPayError::Validation("invalid payslip employee fields".into()))?;
    validate(&fields)?;
    Ok(fields)
}

#[derive(Clone, Debug, async_graphql::SimpleObject)]
pub struct PayslipEmployeeDetail {
    pub field: String,
    pub label: String,
    pub value: String,
}

pub async fn load<C: ConnectionTrait>(
    db: &C,
    tenant: Uuid,
    slip: &payslip::Model,
) -> KabiPayResult<Vec<PayslipEmployeeDetail>> {
    if slip.tenant_id != tenant {
        return Err(KabiPayError::Validation(
            "payslip belongs to another tenant".into(),
        ));
    }
    let setting = payroll_compliance_setting::Entity::find()
        .filter(payroll_compliance_setting::Column::TenantId.eq(tenant))
        .one(db)
        .await?;
    let selected = match setting {
        Some(row) => decode(row.payslip_employee_fields)?,
        None => defaults(),
    };
    if selected.is_empty() {
        return Ok(Vec::new());
    }
    let needs_employee = selected
        .iter()
        .any(|field| !matches!(field.as_str(), "PAYSLIP_STATUS" | "GENERATED_DATE"));
    let row = if needs_employee {
        db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT trim(concat_ws(' ', e.first_name, e.last_name)) AS \"EMPLOYEE_NAME\", \
         e.employee_code AS \"EMPLOYEE_CODE\", d.name AS \"DEPARTMENT\", \
         g.title AS \"DESIGNATION\", to_char(e.date_of_joining, 'DD Mon YYYY') AS \"JOINING_DATE\", \
         e.gender AS \"GENDER\", initcap(replace(e.marital_status, '_', ' ')) AS \"MARITAL_STATUS\", \
         COALESCE(NULLIF($3, ''), e.uan_number) AS \"UAN\", \
         COALESCE(NULLIF($4, ''), e.esic_number) AS \"ESIC\" \
         FROM employee e \
         LEFT JOIN department d ON d.id=e.department_id AND d.tenant_id=e.tenant_id AND NOT d.is_deleted \
         LEFT JOIN designation g ON g.id=e.designation_id AND g.tenant_id=e.tenant_id AND NOT g.is_deleted \
         WHERE e.id=$1 AND e.tenant_id=$2 AND NOT e.is_deleted",
            [
                slip.employee_id.into(),
                tenant.into(),
                slip.uan_number.clone().into(),
                slip.esic_number.clone().into(),
            ],
        ))
        .await?
    } else {
        None
    };
    let mut details = Vec::new();
    for (field, label) in FIELDS {
        if !selected.iter().any(|selected| selected.as_str() == *field) {
            continue;
        }
        let value: Option<String> = match *field {
            "PAYSLIP_STATUS" => Some(slip.status.clone()),
            "GENERATED_DATE" => Some(slip.generated_at.to_rfc3339()),
            _ => match &row {
                Some(row) => row.try_get("", *field)?,
                None => None,
            },
        };
        if let Some(value) = value.filter(|value| !value.trim().is_empty()) {
            details.push(PayslipEmployeeDetail {
                field: (*field).into(),
                label: (*label).into(),
                value,
            });
        }
    }
    Ok(details)
}

#[cfg(test)]
mod tests {
    use super::super::payslip_template_tests::{connection, row_with_fields};
    use super::*;
    use sea_orm::ProxyRow;

    fn slip(tenant: Uuid) -> payslip::Model {
        let now = chrono::Utc::now();
        payslip::Model {
            id: Uuid::new_v4(),
            tenant_id: tenant,
            employee_id: Uuid::new_v4(),
            payroll_cycle_id: Uuid::new_v4(),
            gross_salary: Default::default(),
            total_deductions: Default::default(),
            net_salary: Default::default(),
            pf_employee: None,
            pf_employer: None,
            esi_employee: None,
            esi_employer: None,
            tds_amount: None,
            professional_tax: None,
            uan_number: None,
            esic_number: None,
            status: "GENERATED".into(),
            generated_at: now,
            created_at: now,
            updated_at: now,
        }
    }
    #[test]
    fn employee_fields_default_empty_and_validation() {
        assert_eq!(
            defaults(),
            vec!["EMPLOYEE_NAME", "EMPLOYEE_CODE", "UAN", "ESIC"]
        );
        assert_eq!(decode(serde_json::json!([])).unwrap(), Vec::<String>::new());
        assert!(decode(serde_json::json!(["GENDER", "MARITAL_STATUS", "UAN"])).is_ok());
        assert!(decode(serde_json::json!(["PAYSLIP_STATUS", "GENERATED_DATE"])).is_ok());
        for value in [
            serde_json::json!(["BANK_ACCOUNT"]),
            serde_json::json!(["UAN", "UAN"]),
            serde_json::json!(null),
        ] {
            assert!(decode(value).is_err());
        }
    }

    #[tokio::test]
    async fn only_selected_nonempty_details_are_returned_with_tenant_scoped_joins() {
        let tenant = Uuid::new_v4();
        let slip = slip(tenant);
        let setting = row_with_fields(
            tenant,
            "TABLE",
            vec!["GENDER".into(), "MARITAL_STATUS".into()],
        );
        let employee = ProxyRow::new(std::collections::BTreeMap::from([
            ("GENDER".into(), Some("Female".to_owned()).into()),
            ("MARITAL_STATUS".into(), Some(" ".to_owned()).into()),
            ("UAN".into(), Some("HIDDEN-UAN".to_owned()).into()),
        ]));
        let (db, queries) = connection(vec![vec![setting], vec![employee]], false).await;
        let details = load(&db, tenant, &slip).await.unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].field, "GENDER");
        assert_eq!(details[0].value, "Female");
        let queries = queries.lock().unwrap();
        assert!(queries[0].contains(&tenant.to_string()));
        assert!(queries[1].contains("e.tenant_id="));
        assert!(queries[1].contains("d.tenant_id=e.tenant_id"));
        assert!(queries[1].contains("g.tenant_id=e.tenant_id"));
        assert!(queries[1].contains(&slip.employee_id.to_string()));
        assert!(queries[1].contains(&tenant.to_string()));
    }

    #[tokio::test]
    async fn empty_selection_does_not_fetch_employee_details() {
        let tenant = Uuid::new_v4();
        let (db, queries) =
            connection(vec![vec![row_with_fields(tenant, "TABLE", vec![])]], false).await;
        assert!(load(&db, tenant, &slip(tenant)).await.unwrap().is_empty());
        assert_eq!(queries.lock().unwrap().len(), 1);
    }

    #[tokio::test]
    async fn document_metadata_is_independent_and_does_not_fetch_employee_details() {
        let tenant = Uuid::new_v4();
        let slip = slip(tenant);
        for selected in [
            vec!["PAYSLIP_STATUS".into()],
            vec!["GENERATED_DATE".into()],
            vec!["PAYSLIP_STATUS".into(), "GENERATED_DATE".into()],
        ] {
            let expected_count = selected.len();
            let (db, queries) = connection(
                vec![vec![row_with_fields(tenant, "TABLE", selected)]],
                false,
            )
            .await;
            let details = load(&db, tenant, &slip).await.unwrap();
            assert_eq!(details.len(), expected_count);
            for detail in details {
                match detail.field.as_str() {
                    "PAYSLIP_STATUS" => assert_eq!(detail.value, slip.status),
                    "GENERATED_DATE" => assert_eq!(detail.value, slip.generated_at.to_rfc3339()),
                    _ => panic!("unexpected employee detail"),
                }
            }
            assert_eq!(queries.lock().unwrap().len(), 1);
        }
    }

    #[tokio::test]
    async fn missing_employee_does_not_hide_selected_document_metadata() {
        let tenant = Uuid::new_v4();
        let slip = slip(tenant);
        let setting = row_with_fields(
            tenant,
            "TABLE",
            vec!["EMPLOYEE_NAME".into(), "PAYSLIP_STATUS".into()],
        );
        let (db, _) = connection(vec![vec![setting], vec![]], false).await;
        let details = load(&db, tenant, &slip).await.unwrap();
        assert_eq!(details.len(), 1);
        assert_eq!(details[0].field, "PAYSLIP_STATUS");
        assert_eq!(details[0].value, slip.status);
    }

    #[tokio::test]
    async fn cross_tenant_slip_and_database_failure_never_disclose_details() {
        let tenant = Uuid::new_v4();
        let (db, queries) = connection(vec![], true).await;
        assert!(load(&db, tenant, &slip(Uuid::new_v4())).await.is_err());
        assert!(queries.lock().unwrap().is_empty());
        assert!(load(&db, tenant, &slip(tenant)).await.is_err());
    }
}
