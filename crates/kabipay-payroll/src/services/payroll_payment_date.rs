//! Payment-date readiness is evaluated again when a draft is read or finalized.
use super::payroll_draft::PayrollDraft;
use chrono::NaiveDate;
use kabipay_common::{tenant_business_clock::TenantBusinessClock, KabiPayError, KabiPayResult};
use kabipay_db_entities::tenant::d0012_payroll::payroll_cycle;
use sea_orm::{ConnectionTrait, DbBackend, Statement};

fn date_reason(payment_date: NaiveDate, today: NaiveDate) -> Option<String> {
    (payment_date > today).then(|| format!(
        "Payroll can be finalized on or after {payment_date} in the company timezone. Until then this is a preview."
    ))
}

fn apply_readiness(draft: &mut PayrollDraft, status: &str, reason: Option<String>) {
    draft.can_finalize = status == "DRAFT"
        && reason.is_none()
        && draft.employees.iter().any(|entry| entry.outcome == "READY")
        && !draft
            .employees
            .iter()
            .any(|entry| entry.outcome == "REVIEW");
    draft.finalization_block_reason = reason;
}

pub(crate) async fn refresh<C: ConnectionTrait>(
    db: &C,
    cycle: &payroll_cycle::Model,
    draft: &mut PayrollDraft,
) -> KabiPayResult<()> {
    let reason = if let Some(date) = cycle.payment_date {
        let row = db
            .query_one(Statement::from_sql_and_values(
                DbBackend::Postgres,
                "SELECT timezone FROM kabipay_ops.tenant WHERE id=$1 AND is_deleted=FALSE",
                [cycle.tenant_id.into()],
            ))
            .await?
            .ok_or_else(|| KabiPayError::TenantNotFound(cycle.tenant_id.to_string()))?;
        let timezone: Option<String> = row.try_get("", "timezone")?;
        let clock = TenantBusinessClock::from_configured_name(timezone.as_deref())?;
        date_reason(date, clock.now_date())
    } else {
        None
    };
    apply_readiness(draft, &cycle.status, reason);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::payroll_draft::DraftEmployee;
    use super::*;
    use uuid::Uuid;

    fn date(value: &str) -> NaiveDate {
        value.parse().unwrap()
    }

    fn draft(outcome: &str) -> PayrollDraft {
        PayrollDraft {
            cycle_id: Uuid::new_v4(),
            revision: 1,
            fingerprint: "reviewed-inputs".into(),
            employees: vec![DraftEmployee {
                employee_id: Uuid::new_v4(),
                employee_label: "Employee".into(),
                outcome: outcome.into(),
                reason: None,
                prepared: None,
            }],
            can_finalize: false,
            finalization_block_reason: None,
        }
    }

    #[test]
    fn future_date_blocks_but_payment_day_and_later_allow_finalization() {
        let payment = date("2026-10-10");
        assert!(date_reason(payment, date("2026-10-09"))
            .unwrap()
            .contains("2026-10-10"));
        assert!(date_reason(payment, payment).is_none());
        assert!(date_reason(payment, date("2026-10-11")).is_none());
    }

    #[test]
    fn readiness_uses_company_date_across_utc_midnight() {
        let instant = "2026-10-09T19:00:00Z".parse().unwrap();
        let payment = date("2026-10-10");
        let company = TenantBusinessClock::from_name("Asia/Kolkata").unwrap();
        let utc = TenantBusinessClock::from_name("UTC").unwrap();
        assert!(date_reason(payment, company.business_date(instant)).is_none());
        assert!(date_reason(payment, utc.business_date(instant)).is_some());
    }

    #[test]
    fn saved_preview_becomes_ready_when_date_arrives_without_changing_its_inputs() {
        let mut value = draft("READY");
        let payment = date("2026-10-10");
        apply_readiness(
            &mut value,
            "DRAFT",
            date_reason(payment, date("2026-10-09")),
        );
        assert!(!value.can_finalize);
        let mut loaded: PayrollDraft =
            serde_json::from_value(serde_json::to_value(value).unwrap()).unwrap();
        apply_readiness(&mut loaded, "DRAFT", date_reason(payment, payment));
        assert!(loaded.can_finalize);
        assert!(loaded.finalization_block_reason.is_none());
        assert_eq!(loaded.fingerprint, "reviewed-inputs");
        assert_eq!(loaded.revision, 1);
    }

    #[test]
    fn payment_day_does_not_bypass_employee_review_or_processed_cycle() {
        let mut value = draft("REVIEW");
        apply_readiness(&mut value, "DRAFT", None);
        assert!(!value.can_finalize);
        value.employees[0].outcome = "READY".into();
        apply_readiness(&mut value, "PROCESSED", None);
        assert!(!value.can_finalize);
        apply_readiness(&mut value, "DRAFT", None);
        assert!(value.can_finalize);
        value.employees.clear();
        apply_readiness(&mut value, "DRAFT", None);
        assert!(!value.can_finalize);
    }
}
