use kabipay_payroll::services::{
    arrear_service, contribution_policy_store, payroll_draft, payroll_finalize,
    payroll_period_input, payroll_service, prepare_payroll,
};
use kabipay_tax::services::{tax_history, tax_settings};
use rust_decimal::Decimal;
use sea_orm::{
    ColumnTrait, ConnectionTrait, DatabaseConnection, DbBackend, EntityTrait, QueryFilter,
    Statement, TransactionTrait,
};
use uuid::Uuid;

pub async fn verify(db: &DatabaseConnection, tenant: Uuid, actor: Uuid, employee: Uuid) {
    basic_override(db, tenant, employee).await;
    source_only_component(db, tenant, employee).await;
    for assignment in ["type='DEDUCTION'", "code='RENAMED'", "is_taxable=false"] {
        let tx = db.begin().await.unwrap();
        let error = tx
            .execute_unprepared(&format!(
                "UPDATE salary_component SET {assignment} WHERE code='BASIC'"
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("immutable"), "{error}");
        tx.rollback().await.unwrap();
    }
    let tx = db.begin().await.unwrap();
    let formula = serde_json::json!({"weights":{"BASIC":"1","ARREAR":"1","OVERTIME":"1","INCENTIVE":"1"},"rate":"0.12","ceiling":null,"rounding":"HALF_UP_2DP"});
    let policy = serde_json::json!({"effective_from":"2026-10-01","effective_until":null,"lwp_divisor":31,"origin":"HR_CONFIGURATION","reason":"Synthetic reviewed earnings basis","pf_employee":formula,"pf_employer":formula,"esi_basis":formula,"esi_employer_rate":"0.0325","company_esi_covered":false,"esi_mode":"CUSTOM_COMPONENTS","classifications":{},"professional_tax":"200.00"});
    contribution_policy_store::save(
        &tx,
        tenant,
        actor,
        serde_json::from_value(policy).unwrap(),
        None,
    )
    .await
    .unwrap();
    let settings = serde_json::json!({"regime":"NEW","method":"PERCENTAGE_OVERRIDE","percentage":"0.10","basis_components":["BASIC","HRA","CONVEYANCE","OTHER","ARREAR","OVERTIME","INCENTIVE"],"effective_from":"2026-10-01","effective_until":null,"resident":true,"reason":"Synthetic reviewed withholding basis"});
    tax_settings::save_tax_settings(
        &tx,
        tenant,
        actor,
        employee,
        serde_json::from_value(settings).unwrap(),
        None,
    )
    .await
    .unwrap();
    let input = serde_json::from_value(serde_json::json!({"year":2026,"month":11,"gross_rule":"FIXED_MINUS_LWP","fixed_gross":"30000","lwp_days":"0","lwp_divisor":"31","lwp_basis":"GROSS","lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION","variable_allowance_ot":"1000","incentive":"500","advance_already_paid":"0","additional_deductions":[],"statutory_overrides":{},"expected_earned_components":{},"expected_employer_contributions":{},"expected_statement":{},"historical_lwp_included":false,"ready":true,"automatic":{"eligibility":{"pf_applicable":true,"esi_applicable":false,"esi_continuation_until":null,"disability":false,"average_daily_wage":null},"withholding_override":null}})).unwrap();
    let arrear = arrear_service::create_arrear(
        &tx,
        tenant,
        employee,
        Decimal::from(1000),
        Some("Synthetic catch-up".into()),
    )
    .await
    .unwrap();
    let prepared = prepare_payroll::prepare(&tx, tenant, employee, &input)
        .await
        .unwrap();
    assert_eq!(prepared.calculation.components["ARREAR"], "1000.00");
    assert_eq!(prepared.calculation.components["OVERTIME"], "1000.00");
    assert_eq!(prepared.calculation.statutory["TDS"], "3250.00");
    assert_eq!(prepared.calculation.statutory["PF"], "2100.00");
    assert_eq!(prepared.arrears.len(), 1);
    assert_eq!(prepared.arrears[0].id, arrear.id);
    tx.execute_unprepared("UPDATE salary_component SET is_active=false WHERE code='OVERTIME'")
        .await
        .unwrap();
    assert!(prepare_payroll::prepare(&tx, tenant, employee, &input)
        .await
        .unwrap_err()
        .to_string()
        .contains("OVERTIME"));
    tx.execute_unprepared("UPDATE salary_component SET is_active=true WHERE code='OVERTIME'")
        .await
        .unwrap();
    payroll_period_input::save(&tx, tenant, actor, employee, input, None, None)
        .await
        .unwrap();
    tx.commit().await.unwrap();

    let cycle = payroll_service::create_payroll_cycle(
        db,
        tenant,
        "Arrears and overtime regression".into(),
        11,
        2026,
        None,
    )
    .await
    .unwrap();
    let first = payroll_draft::calculate_payroll_cycle(db, tenant, actor, cycle.id, None)
        .await
        .unwrap();
    let draft =
        payroll_draft::calculate_payroll_cycle(db, tenant, actor, cycle.id, Some(first.revision))
            .await
            .unwrap();
    assert!(draft.can_finalize, "{:?}", draft.employees);
    assert_eq!(
        arrear_service::list_pending_by_employee(db, tenant, employee)
            .await
            .unwrap()
            .len(),
        1
    );
    let ack = payroll_draft::FinalizeAcknowledgement {
        provisional_tax_employees: draft
            .employees
            .iter()
            .filter(|e| {
                e.prepared
                    .as_ref()
                    .is_some_and(|p| p.requires_tax_acknowledgement)
            })
            .map(|e| e.employee_id)
            .collect(),
    };
    payroll_finalize::finalize_payroll_cycle(
        db,
        tenant,
        actor,
        cycle.id,
        draft.revision,
        &draft.fingerprint,
        ack.clone(),
    )
    .await
    .unwrap();
    assert!(
        arrear_service::list_pending_by_employee(db, tenant, employee)
            .await
            .unwrap()
            .is_empty()
    );
    assert!(payroll_finalize::finalize_payroll_cycle(
        db,
        tenant,
        actor,
        cycle.id,
        draft.revision,
        &draft.fingerprint,
        ack
    )
    .await
    .is_err());
    let row = db
        .query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT status,applied_payroll_cycle_id FROM payroll_arrear WHERE id=$1",
            [arrear.id.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    assert_eq!(row.try_get::<String>("", "status").unwrap(), "APPLIED");
    assert_eq!(
        row.try_get::<Uuid>("", "applied_payroll_cycle_id").unwrap(),
        cycle.id
    );
    export_history(db, tenant, actor, employee).await;
}

async fn source_only_component(db: &DatabaseConnection, tenant: Uuid, employee: Uuid) {
    for assignment in ["type='DEDUCTION'", "code='RENAMED'", "is_taxable=false"] {
        let tx = db.begin().await.unwrap();
        let component = kabipay_tenant_import::salary_import::ensure_component(
            &tx,
            tenant,
            "SOURCE_ONLY",
            "EARNING",
        )
        .await
        .unwrap();
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO payroll_period_input(id,tenant_id,employee_id,year,month,input,ready,revision,updated_by) SELECT gen_random_uuid(),tenant_id,employee_id,2026,12,jsonb_set(input,'{expected_earned_components}', '{\"SOURCE_ONLY\":\"30000.00\"}'::jsonb),true,1,updated_by FROM payroll_period_input WHERE tenant_id=$1 AND employee_id=$2 AND year=2026 AND month=9",
            [tenant.into(),employee.into()])).await.unwrap();
        kabipay_payroll::services::component_display::save(&tx, tenant, component, false)
            .await
            .unwrap();
        let error = tx
            .execute_unprepared(&format!(
                "UPDATE salary_component SET {assignment} WHERE code='SOURCE_ONLY'"
            ))
            .await
            .unwrap_err();
        assert!(error.to_string().contains("immutable"), "{error}");
        tx.rollback().await.unwrap();
    }
}

async fn basic_override(db: &DatabaseConnection, tenant: Uuid, employee: Uuid) {
    use kabipay_db_entities::tenant::d0012_payroll::employee_salary_structure as assignment;
    let tx = db.begin().await.unwrap();
    let assigned = assignment::Entity::find()
        .filter(assignment::Column::TenantId.eq(tenant))
        .filter(assignment::Column::EmployeeId.eq(employee))
        .one(&tx)
        .await
        .unwrap()
        .unwrap();
    tx.execute_unprepared("UPDATE salary_structure_component SET calculation_basis='FIXED_MONTHLY',calculation_value=10000 WHERE salary_component_id IN(SELECT id FROM salary_component WHERE code='BASIC'); UPDATE salary_structure_component SET calculation_basis='PERCENT_OF_BASIC',calculation_value=50 WHERE salary_component_id IN(SELECT id FROM salary_component WHERE code='HRA')").await.unwrap();
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,"INSERT INTO employee_salary_component_override(id,tenant_id,employee_salary_structure_id,salary_component_id,calculation_basis,calculation_value,is_active) SELECT gen_random_uuid(),$1,$2,id,'FIXED_MONTHLY',20000,true FROM salary_component WHERE tenant_id=$1 AND code='BASIC'",[tenant.into(),assigned.id.into()])).await.unwrap();
    let breakup = kabipay_common::salary_breakup::salary_breakup_for_structure(
        &tx,
        tenant,
        employee,
        assigned.clone(),
        "BASIC",
        Decimal::ZERO,
    )
    .await
    .unwrap();
    assert_eq!(
        breakup
            .lines
            .iter()
            .find(|l| l.component_code == "BASIC")
            .unwrap()
            .monthly_amount,
        Decimal::from(20000)
    );
    assert_eq!(
        breakup
            .lines
            .iter()
            .find(|l| l.component_code == "HRA")
            .unwrap()
            .monthly_amount,
        Decimal::from(10000)
    );
    tx.execute_unprepared(
        "UPDATE employee_salary_component_override SET calculation_basis='PERCENT_OF_BASIC'",
    )
    .await
    .unwrap();
    assert!(
        kabipay_common::salary_breakup::salary_breakup_for_structure(
            &tx,
            tenant,
            employee,
            assigned,
            "BASIC",
            Decimal::ZERO
        )
        .await
        .is_err()
    );
    tx.rollback().await.unwrap();
}

async fn export_history(db: &DatabaseConnection, tenant: Uuid, actor: Uuid, employee: Uuid) {
    let tx = db.begin().await.unwrap();
    let entry = serde_json::from_value(serde_json::json!({"fiscal_year":2026,"period_start":"2026-04-01","period_end":"2026-08-31","employer":"CURRENT","source_key":"release-opening","earnings":"100000.00","components":{"BASIC":"100000.00"},"tds":null,"coverage":"INCOMPLETE","reason":"Synthetic earnings evidence without TDS","evidence":"IMPORTED_ACTUAL"})).unwrap();
    tax_history::save_tax_history(&tx, tenant, actor, employee, entry, None)
        .await
        .unwrap();
    let totals =
        kabipay_payroll::services::payroll_export_evidence::supplements(&tx, tenant, 2026, None)
            .await
            .unwrap();
    assert_eq!(totals[&employee].gross, Decimal::from(100000));
    assert_eq!(totals[&employee].unknown_tds, 1);
    assert_eq!(
        totals[&employee].source_count, 0,
        "finalized source months must not be counted twice"
    );
    assert!(
        kabipay_payroll::services::payroll_export_evidence::supplements(&tx, tenant, 2026, Some(2))
            .await
            .is_err()
    );
    tx.commit().await.unwrap();
    let csv = payroll_service::india_fy_payroll_employee_totals_csv(db, tenant, 2026)
        .await
        .unwrap();
    let mut rows = csv.lines();
    let header: Vec<_> = rows.next().unwrap().split(',').collect();
    let employee = rows
        .find(|r| r.contains("EXAMPLE-001"))
        .unwrap()
        .split(',')
        .collect::<Vec<_>>();
    let field = |name| employee[header.iter().position(|h| *h == name).unwrap()];
    // Opening 100,000 + September 30,500 + October 29,532.26 + November 32,500.
    assert_eq!(field("sum_gross_salary").trim_matches('"'), "192532.26");
    assert_eq!(field("sum_tds_amount"), "");
    assert_eq!(field("opening_history_rows"), "1");
}
