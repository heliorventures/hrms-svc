//! Validated company-wide presentation choice. Payroll calculations do not depend on it.
use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::payroll_compliance_setting;
use sea_orm::{ColumnTrait, ConnectionTrait, EntityTrait, QueryFilter};
use uuid::Uuid;

pub fn resolve_payslip_template(
    current: Option<&str>,
    requested: Option<&str>,
) -> KabiPayResult<String> {
    match requested.or(current).unwrap_or("EXISTING") {
        value @ ("EXISTING" | "TABLE") => Ok(value.to_owned()),
        _ => Err(KabiPayError::Validation(
            "unsupported company payslip template".into(),
        )),
    }
}

pub async fn load<C: ConnectionTrait>(db: &C, tenant: Uuid) -> KabiPayResult<String> {
    let row = payroll_compliance_setting::Entity::find()
        .filter(payroll_compliance_setting::Column::TenantId.eq(tenant))
        .one(db)
        .await?;
    resolve_payslip_template(
        row.as_ref().map(|value| value.payslip_template.as_str()),
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::resolve_payslip_template;

    #[test]
    fn payslip_template_defaults_only_when_unconfigured() {
        assert_eq!(resolve_payslip_template(None, None).unwrap(), "EXISTING");
        assert_eq!(
            resolve_payslip_template(Some("TABLE"), None).unwrap(),
            "TABLE"
        );
    }

    #[test]
    fn payslip_template_accepts_explicit_changes_and_rejects_unknown_values() {
        for value in ["EXISTING", "TABLE"] {
            assert_eq!(
                resolve_payslip_template(Some("TABLE"), Some(value)).unwrap(),
                value
            );
        }
        for value in ["", "table", "UNKNOWN", " TABLE "] {
            assert!(resolve_payslip_template(None, Some(value)).is_err());
            assert!(resolve_payslip_template(Some(value), None).is_err());
        }
    }
}
