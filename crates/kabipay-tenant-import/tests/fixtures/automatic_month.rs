use kabipay_payroll::services::{
    employee_eligibility, payroll_draft, payroll_finalize, payroll_period_input, payroll_service,
};
use sea_orm::{DatabaseConnection, TransactionTrait};
use uuid::Uuid;

pub async fn verify(db: &DatabaseConnection, tenant: Uuid, actor: Uuid, employee: Uuid) {
    assert!(payroll_period_input::find(db, tenant, employee, 2026, 12)
        .await
        .unwrap()
        .is_none());
    let cycle = payroll_service::create_payroll_cycle(
        db,
        tenant,
        "Automatic month regression".into(),
        12,
        2026,
        None,
    )
    .await
    .unwrap();
    let blocked = payroll_draft::calculate_payroll_cycle(db, tenant, actor, cycle.id, None)
        .await
        .unwrap();
    assert!(!blocked.can_finalize);
    assert!(blocked
        .employees
        .iter()
        .any(|row| row.employee_id == employee
            && row
                .reason
                .as_deref()
                .is_some_and(|reason| reason.contains("eligibility"))));
    let tx = db.begin().await.unwrap();
    let setting = serde_json::from_value(serde_json::json!({
        "effective_from":"2026-12-01", "reason":"Synthetic confirmed eligibility",
        "eligibility":{"pf_applicable":true,"esi_applicable":false,"disability":false,"esi_continuation_until":null,"average_daily_wage":null}
    })).unwrap();
    employee_eligibility::save(&tx, tenant, employee, actor, setting)
        .await
        .unwrap();
    tx.commit().await.unwrap();
    let initial =
        payroll_draft::calculate_payroll_cycle(db, tenant, actor, cycle.id, Some(blocked.revision))
            .await
            .unwrap();
    assert!(initial.can_finalize, "{:?}", initial.employees);
    let prepared = initial
        .employees
        .iter()
        .find(|row| row.employee_id == employee)
        .unwrap()
        .prepared
        .as_ref()
        .unwrap();
    assert_eq!(
        prepared.calculation.gross,
        rust_decimal::Decimal::from(30000)
    );
    assert_eq!(prepared.calculation.statutory["PF"], "1800.00");
    assert_eq!(prepared.calculation.statutory["TDS"], "3000.00");
    let row = payroll_period_input::find(db, tenant, employee, 2026, 12)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.input["fixed_gross"],prepared.input.fixed_gross.clone().unwrap());
    assert_eq!(row.input["lwp_days"],prepared.input.lwp_days.clone().unwrap());
    let mut input: kabipay_payroll::services::payroll_rules::PeriodInput =
        serde_json::from_value(row.input).unwrap();
    input.incentive = Some("500".into());
    input.advance_already_paid = Some("1000".into());
    let tx = db.begin().await.unwrap();
    payroll_period_input::save(
        &tx,
        tenant,
        actor,
        employee,
        input,
        None,
        Some(row.revision),
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let draft =
        payroll_draft::calculate_payroll_cycle(db, tenant, actor, cycle.id, Some(initial.revision))
            .await
            .unwrap();
    let prepared = draft
        .employees
        .iter()
        .find(|row| row.employee_id == employee)
        .unwrap()
        .prepared
        .as_ref()
        .unwrap();
    assert_eq!(
        prepared.calculation.incentive,
        rust_decimal::Decimal::from(500)
    );
    assert_eq!(
        prepared.calculation.net_earned - prepared.calculation.remaining_payable,
        rust_decimal::Decimal::from(1000)
    );
    let ack = payroll_draft::FinalizeAcknowledgement {
        provisional_tax_employees: draft
            .employees
            .iter()
            .filter(|row| {
                row.prepared
                    .as_ref()
                    .is_some_and(|p| p.requires_tax_acknowledgement)
            })
            .map(|row| row.employee_id)
            .collect(),
    };
    payroll_finalize::finalize_payroll_cycle(
        db,
        tenant,
        actor,
        cycle.id,
        draft.revision,
        &draft.fingerprint,
        ack,
    )
    .await
    .unwrap();
    assert!(payroll_draft::calculate_payroll_cycle(
        db,
        tenant,
        actor,
        cycle.id,
        Some(draft.revision)
    )
    .await
    .is_err());
    let setting = employee_eligibility::find(db, tenant, employee, "2027-01-01".parse().unwrap())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(setting.eligibility.pf_applicable, Some(true));
    let tx = db.begin().await.unwrap();
    assert!(
        employee_eligibility::save(&tx, tenant, employee, actor, setting)
            .await
            .is_err()
    );
    tx.rollback().await.unwrap();
}
