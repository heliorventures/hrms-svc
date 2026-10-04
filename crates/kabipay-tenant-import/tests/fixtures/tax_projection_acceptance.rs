use kabipay_payroll::services::{contribution_policy_store, prepare_payroll};
use kabipay_tax::services::{approved_deductions, tax_history, tax_projection, tax_settings};
use rust_decimal::Decimal;
use sea_orm::{ConnectionTrait, DatabaseConnection, DbBackend, Statement, TransactionTrait};
use uuid::Uuid;

pub async fn verify(db: &DatabaseConnection, tenant: Uuid, actor: Uuid, employee: Uuid) {
    let tx = db.begin().await.unwrap();
    let formula = serde_json::json!({"weights":{"BASIC":"1"},"rate":"0.12","ceiling":null,"rounding":"HALF_UP_2DP"});
    let policy = serde_json::from_value(serde_json::json!({"effective_from":"2026-10-01","effective_until":null,"lwp_divisor":31,
        "origin":"HR_CONFIGURATION","reason":"Synthetic reviewed formula","pf_employee":formula,"pf_employer":formula,
        "esi_basis":formula,"esi_employer_rate":"0.0325","company_esi_covered":false,"esi_mode":"CUSTOM_COMPONENTS","classifications":{},"professional_tax":"200.00"})).unwrap();
    contribution_policy_store::save(&tx, tenant, actor, policy, None)
        .await
        .unwrap();
    let settings = serde_json::from_value(serde_json::json!({"regime":"NEW","method":"PERCENTAGE_OVERRIDE","percentage":"0.10","basis_components":["BASIC","HRA","CONVEYANCE","OTHER"],"effective_from":"2026-10-01","effective_until":null,"resident":true,"reason":"Synthetic HR instruction"})).unwrap();
    tax_settings::save_tax_settings(&tx, tenant, actor, employee, settings, None)
        .await
        .unwrap();
    let configured = tx.query_one(Statement::from_sql_and_values(DbBackend::Postgres,
        "SELECT COUNT(*) AS n FROM tax_configuration_version WHERE tenant_id=$1 AND fiscal_year=2026 AND regime='NEW' AND is_active=true", [tenant.into()])).await.unwrap().unwrap();
    assert_eq!(configured.try_get::<i64>("", "n").unwrap(), 1, "normal employee tax configuration must establish the declaration/proof association");
    let context = kabipay_tax::services::tax_submission::context(&tx, tenant, employee, 2026).await.unwrap().unwrap();
    assert_eq!(context.settings.regime, kabipay_tax::domain::TaxRegime::New);
    let version = kabipay_tax::services::tax_submission::resolve_version(&tx, tenant, employee, 2026, None, Some("NEW")).await.unwrap();
    let repeat = kabipay_tax::services::tax_submission::resolve_version(&tx, tenant, employee, 2026, None, Some("NEW_REGIME")).await.unwrap();
    assert_eq!(version.id, repeat.id);
    assert!(kabipay_tax::services::tax_submission::resolve_version(&tx, tenant, employee, 2026, None, Some("OLD")).await.is_err());
    assert!(kabipay_tax::services::tax_submission::context(&tx, Uuid::new_v4(), employee, 2026).await.is_err());
    kabipay_tax::services::tax_declarations::save_declaration(&tx, tenant, employee, version.id, 2026, Some("NEW".into()), Some(Decimal::from(360000)), Some(Decimal::from(1000))).await.unwrap();
    let saved = kabipay_tax::services::tax_submission::context(&tx, tenant, employee, 2026).await.unwrap().unwrap().declaration.unwrap();
    assert_eq!(saved["input"]["gross_income"], "360000");
    assert_eq!(saved["input"]["regime"], "NEW");
    let future = kabipay_tax::services::tax_submission::resolve_version(&tx, tenant, employee, 2027, None, None).await.unwrap();
    assert_eq!(future.fiscal_year, 2027);
    assert_ne!(future.id, version.id);
    let input = serde_json::from_value(serde_json::json!({"year":2026,"month":10,"gross_rule":"FIXED_MINUS_LWP","fixed_gross":"30000",
        "lwp_days":"1","lwp_divisor":"31","lwp_basis":"GROSS","lwp_handling":"SOURCE_GROSS_INCLUDES_REDUCTION",
        "variable_allowance_ot":"0","incentive":"1000","advance_already_paid":"5000","additional_deductions":[],
        "statutory_overrides":{},"expected_earned_components":{},"expected_employer_contributions":{},"expected_statement":{},
        "historical_lwp_included":false,"ready":true,"automatic":{"eligibility":{"pf_applicable":false,"esi_applicable":false,"esi_continuation_until":null,"disability":false,"average_daily_wage":null},"withholding_override":null}})).unwrap();
    let prepared = prepare_payroll::prepare(&tx, tenant, employee, &input)
        .await
        .unwrap();
    assert_eq!(
        prepared.calculation.net_earned - prepared.calculation.remaining_payable,
        Decimal::from(5000)
    );
    assert!(prepared.requires_tax_acknowledgement);
    assert!(prepared.contribution_policy.is_some());
    assert_eq!(
        prepared.calculation.statutory["TDS"]
            .parse::<Decimal>()
            .unwrap(),
        (prepared.calculation.gross * Decimal::new(10, 2)).round_dp(2)
    );
    let projection = prepared.tax_projection.unwrap();
    assert!(!projection.history_complete);
    assert!(projection.withholding.unwrap().remaining.is_none());
    let previous = serde_json::from_value(serde_json::json!({
        "fiscal_year":2026,"period_start":"2026-04-01","period_end":"2026-04-30",
        "employer":"PREVIOUS","source_key":"partial-previous","earnings":"10000.00",
        "components":{"BASIC":"10000.00"},"tds":"0.00","coverage":"INCOMPLETE",
        "reason":"Synthetic partial evidence","evidence":"IMPORTED_ACTUAL"
    }))
    .unwrap();
    tax_history::save_tax_history(&tx, tenant, actor, employee, previous, None)
        .await
        .unwrap();
    let loaded = tax_projection::load_projection_input(&tx, tenant, employee, 2026, 10)
        .await
        .unwrap();
    assert_eq!(loaded.previous_employer_tds, Some(Decimal::ZERO));
    assert_eq!(loaded.previous_employer_history_complete, Some(false));
    let old = Uuid::new_v4();
    let new = Uuid::new_v4();
    let revision = Uuid::new_v4();
    for (id, regime) in [(old, "OLD_REGIME"), (revision, "OLD"), (new, "NEW_REGIME")] {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO tax_configuration_version(id,tenant_id,fiscal_year,regime,country_code,is_active) VALUES($1,$2,2026,$3,'IN',false)",
            [id.into(),tenant.into(),regime.into()])).await.unwrap();
    }
    for (year, regime, country) in [(2027, "OLD_REGIME", "IN"), (2026, "NEW", "IN"), (2026, "OLD", "US")] {
        assert!(kabipay_tax::services::tax_service::upsert_tax_configuration_version(&tx, tenant, Some(old), year, Some(regime.into()), country.into(), true).await.is_err(), "a referenced definition's financial year, regime and country identity must be immutable");
    }
    for (id, status, amount, offset) in [
        (old, "APPROVED", 1000, 0),
        (revision, "PENDING", 1500, 1),
        (new, "APPROVED", 9000, 2),
    ] {
        tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
            "INSERT INTO tax_proof_line(id,tenant_id,employee_id,tax_config_version_id,fiscal_year,section_code,declared_amount,actual_amount,status,submitted_at) VALUES(gen_random_uuid(),$1,$2,$3,2026,'80C',$4,$4,$5,NOW()+$6*INTERVAL '1 second')",
            [tenant.into(),employee.into(),id.into(),Decimal::from(amount).into(),status.into(),offset.into()])).await.unwrap();
    }
    assert_eq!(
        approved_deductions::old_regime_total(&tx, tenant, employee, 2026)
            .await
            .unwrap(),
        Decimal::ZERO
    );
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tax_proof_line SET status='APPROVED' WHERE tax_config_version_id=$1",
        [revision.into()],
    ))
    .await
    .unwrap();
    assert_eq!(
        approved_deductions::old_regime_total(&tx, tenant, employee, 2026)
            .await
            .unwrap(),
        Decimal::from(1500)
    );
    // Reviewing an older proof later cannot supersede the newer submission.
    tx.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE tax_proof_line SET status='APPROVED' WHERE tax_config_version_id=$1",
        [old.into()],
    ))
    .await
    .unwrap();
    assert_eq!(
        approved_deductions::old_regime_total(&tx, tenant, employee, 2026)
            .await
            .unwrap(),
        Decimal::from(1500)
    );
    tx.execute(Statement::from_sql_and_values(DbBackend::Postgres,
        "UPDATE tax_proof_line SET submitted_at=NOW()+INTERVAL '1 second' WHERE tax_config_version_id=$1", [old.into()])).await.unwrap();
    assert!(
        approved_deductions::old_regime_total(&tx, tenant, employee, 2026)
            .await
            .is_err(),
        "ambiguous submissions must not select a random UUID"
    );
    tx.rollback().await.unwrap();
}
