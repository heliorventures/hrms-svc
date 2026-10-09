//! Runs only against the disposable database created by scripts/test-loans.ps1.
use chrono::{Months, NaiveDate, Utc};
use kabipay_common::context::{ClientClaims, CLIENT_JWT_ISSUER};
use kabipay_loans::*;
use kabipay_loans_domain::*;
use rust_decimal::Decimal;
use sea_orm::{
    ConnectOptions, ConnectionTrait, Database, DatabaseConnection, DbBackend, Statement,
    TransactionTrait, Value,
};
use std::collections::HashMap;
use uuid::Uuid;

struct Fixture {
    db: DatabaseConnection,
    tenant: Uuid,
    employee: Uuid,
    user: Uuid,
    other: Uuid,
    other_user: Uuid,
    manager: Uuid,
    workflow: Uuid,
    step: Uuid,
    policy: Uuid,
    today: NaiveDate,
}
async fn sql(db: &DatabaseConnection, source: &str, values: Vec<Value>) {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        source,
        values,
    ))
    .await
    .unwrap();
}
impl Fixture {
    async fn new(company_limit: i64) -> Self {
        let url = std::env::var("LOAN_TEST_DATABASE_URL").expect("use scripts/test-loans.ps1");
        assert!(
            url.starts_with("postgresql://loan_fixture:loan_fixture@127.0.0.1:")
                && url.ends_with("/loan_phase_a"),
            "refusing a non-fixture database"
        );
        let mut options = ConnectOptions::new(url);
        options
            .set_schema_search_path("loan_test,public")
            .sqlx_logging(false);
        let db = Database::connect(options).await.unwrap();
        let mut f = Self {
            db,
            tenant: Uuid::new_v4(),
            employee: Uuid::new_v4(),
            user: Uuid::new_v4(),
            other: Uuid::new_v4(),
            other_user: Uuid::new_v4(),
            manager: Uuid::new_v4(),
            workflow: Uuid::new_v4(),
            step: Uuid::new_v4(),
            policy: Uuid::nil(),
            today: Utc::now().date_naive(),
        };
        sql(&f.db,"INSERT INTO kabipay_ops.tenant(id,name,status,timezone,currency,is_deleted) VALUES($1,'loan test','ACTIVE','UTC','INR',FALSE)",vec![f.tenant.into()]).await;
        for code in ["LOANS", "PAYROLL"] {
            sql(&f.db,"INSERT INTO kabipay_ops.module(id,code,name,is_active,is_core) VALUES($1,$2,$2,TRUE,TRUE)",vec![Uuid::new_v4().into(),code.into()]).await;
        }
        for (employee, user) in [
            (f.employee, f.user),
            (f.other, f.other_user),
            (Uuid::new_v4(), f.manager),
        ] {
            sql(&f.db,"INSERT INTO employee(id,tenant_id,user_id,employment_type,date_of_joining,status,is_deleted) VALUES($1,$2,$3,'FULL_TIME','2020-01-01','ACTIVE',FALSE)",vec![employee.into(),f.tenant.into(),user.into()]).await;
        }
        sql(&f.db,"INSERT INTO workflow(id,tenant_id,name,entity_type,is_active) VALUES($1,$2,'Loan test','LOAN_REQUEST',TRUE)",vec![f.workflow.into(),f.tenant.into()]).await;
        sql(&f.db,"INSERT INTO workflow_step(id,tenant_id,workflow_id,sequence_order,step_name,approver_type,approver_permission,can_skip) VALUES($1,$2,$3,1,'Finance approval','PERMISSION','loan:approve',FALSE)",vec![f.step.into(),f.tenant.into(),f.workflow.into()]).await;
        let rules = LoanPolicy {
            eligibility: EligibilityPolicy {
                employment_types: vec!["FULL_TIME".into()],
                minimum_service_days: 30,
            },
            exposure: ExposurePolicy {
                per_loan: Decimal::from(10000),
                per_employee: Decimal::from(10000),
                per_company: Decimal::from(company_limit),
            },
            interest: InterestPolicy {
                methods: vec![
                    InterestKind::InterestFree,
                    InterestKind::OneTimePercentage,
                    InterestKind::OneTimeFixed,
                    InterestKind::ReducingBalance,
                ],
                maximum_rate: Decimal::from(20),
                maximum_fixed_charge: Decimal::from(1000),
            },
            recovery: RecoveryPolicy {
                modes: vec![
                    RecoveryMode::Payroll,
                    RecoveryMode::External,
                    RecoveryMode::Mixed,
                ],
                priority: RecoveryPriority::OldestApprovedFirst,
                minimum_monthly_amount: Decimal::from(100),
                max_instalments: 24,
                allow_residual: false,
                allow_overrides: true,
                allow_skips: true,
                allow_accrual_pause: true,
                minimum_net_pay: Decimal::from(1000),
                maximum_net_pay_percentage: Decimal::from(50),
            },
            short_salary: ShortSalaryPolicy::CapAndCarry,
            allocation: AllocationOrder::InterestFirst,
            excess_credit: ExcessCreditPolicy::HoldForReview,
            early_settlement: EarlySettlementPolicy::KeepAssessedCharge,
            exit_recovery: ExitRecoveryPolicy {
                allow_fnf: true,
                allow_continuing: true,
                review_reference: "fixture-reviewed-setoff".into(),
            },
            tax_treatment: TaxTreatmentPolicy {
                jurisdiction: "fixture-only".into(),
                review_reference: "fixture-reviewed-tax".into(),
                external_assessment_required: true,
            },
            approval: ApprovalPolicy {
                workflow_id: f.workflow.to_string(),
                acknowledgement_required: true,
            },
        };
        f.policy = f
            .apply(
                f.manager,
                "ALL",
                1,
                "policy",
                LoanCommand::PublishPolicy {
                    key: "fixture".into(),
                    currency: f.currency(),
                    effective_from: f.today,
                    effective_to: None,
                    rules,
                },
            )
            .await
            .unwrap()
            .record_id;
        f
    }
    fn currency(&self) -> Currency {
        Currency {
            code: "INR".into(),
            minor_units: 2,
        }
    }
    fn claims(&self, user: Uuid, scope: &str, permission: LoanPermission) -> ClientClaims {
        ClientClaims {
            sub: user,
            tenant_id: self.tenant,
            iss: CLIENT_JWT_ISSUER.into(),
            exp: 0,
            iat: 0,
            email: String::new(),
            employee_id: Some(self.other),
            must_change_password: false,
            roles: vec!["ADMIN".into()],
            permissions: vec![permission.wire().into()],
            permission_scopes: HashMap::from([(permission.wire().into(), scope.into())]),
            resource_scopes: HashMap::new(),
        }
    }
    fn actor(&self, user: Uuid, scope: &str, permission: LoanPermission) -> LoanActorScope {
        LoanActorScope::from_verified_claims(
            &self.claims(user, scope, permission),
            self.tenant,
            permission,
        )
        .unwrap()
    }
    async fn apply(
        &self,
        user: Uuid,
        scope: &str,
        version: i64,
        key: &str,
        command: LoanCommand,
    ) -> LoanResult<LoanCommandResult> {
        let actor = self.actor(user, scope, command.permission());
        let tx = self.db.begin().await.unwrap();
        let result = execute_command(
            &tx,
            &actor,
            &CommandMeta {
                idempotency_key: key.into(),
                expected_version: version,
            },
            &command,
        )
        .await;
        match result {
            Ok(result) => {
                tx.commit().await.unwrap();
                Ok(result)
            }
            Err(error) => {
                tx.rollback().await.unwrap();
                Err(error)
            }
        }
    }
    async fn request(&self, user: Uuid, key: &str) -> LoanCommandResult {
        self.apply(
            user,
            "SELF",
            1,
            key,
            LoanCommand::SubmitRequest {
                request_id: None,
                employee_id: None,
                policy_id: self.policy,
                amount: Decimal::from(10000),
                purpose: "Fixture purpose".into(),
                notes: None,
                preferences: serde_json::json!({}),
            },
        )
        .await
        .unwrap()
    }
    fn terms(&self, interest: InterestMethod) -> LoanTerms {
        LoanTerms {
            currency: self.currency(),
            approved_principal: Decimal::from(10000),
            interest,
            rounding: RoundingRule::HalfUp,
            allocation: AllocationOrder::InterestFirst,
            recovery: RecoveryMode::Mixed,
            monthly_amount: Decimal::from(500),
            max_instalments: 24,
            allow_residual: false,
            calculator_version: CALCULATOR_VERSION.into(),
        }
    }
    fn approval(&self, request: Uuid, interest: InterestMethod) -> LoanCommand {
        LoanCommand::DecideRequest {
            request_id: request,
            step_id: self.step,
            decision: RequestDecision::Approve,
            reason: "Reviewed fixture".into(),
            terms: Some(self.terms(interest)),
            effective_from: Some(self.today),
            first_due_date: Some(self.today.checked_add_months(Months::new(1)).unwrap()),
            agreement_reference: Some("fixture-employee-agreement".into()),
        }
    }
    async fn loan(&self, interest: InterestMethod) -> Uuid {
        let request = self.request(self.user, "request").await;
        self.apply(
            self.manager,
            "ALL",
            request.version,
            "approve",
            self.approval(request.record_id, interest),
        )
        .await
        .unwrap()
        .loan_id
        .unwrap()
    }
    fn payment(&self, amount: i64) -> RecordedPayment {
        RecordedPayment {
            amount: Decimal::from(amount),
            value_date: self.today,
            method: "EXTERNAL_BANK".into(),
            external_reference: format!("fixture-{amount}"),
            evidence_reference: "fixture-bank-evidence".into(),
        }
    }
    async fn account(&self, loan: Uuid) -> LoanAccountView {
        let tx = self.db.begin().await.unwrap();
        let result = loan_account(
            &tx,
            &self.actor(self.user, "SELF", LoanPermission::Read),
            loan,
        )
        .await
        .unwrap();
        tx.commit().await.unwrap();
        result
    }
    fn recovery(&self, source: Uuid, amount: i64) -> RecoveryInput {
        RecoveryInput {
            employee_id: self.employee,
            source: RecoverySource::Fnf,
            source_id: source,
            source_revision: 1,
            period_start: self.today.with_day(1).unwrap(),
            value_date: self.today,
            eligible_net: Decimal::from(amount),
            currency: self.currency(),
        }
    }
}
use chrono::Datelike;

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn self_identity_scope_and_self_approval_are_enforced_from_current_employee_links() {
    let f = Fixture::new(100000).await;
    let request = f.request(f.user, "request").await;
    assert_eq!(request.employee_id, Some(f.employee));
    let self_decision = f
        .apply(
            f.user,
            "ALL",
            request.version,
            "self-approve",
            f.approval(request.record_id, InterestMethod::InterestFree),
        )
        .await;
    assert!(matches!(self_decision, Err(LoanModuleError::Authority(_))));
    let other = f
        .apply(
            f.user,
            "SELF",
            1,
            "spoof",
            LoanCommand::SubmitRequest {
                request_id: None,
                employee_id: Some(f.other),
                policy_id: f.policy,
                amount: Decimal::from(1000),
                purpose: "spoof".into(),
                notes: None,
                preferences: serde_json::json!({}),
            },
        )
        .await;
    assert!(matches!(other, Err(LoanModuleError::Forbidden)));
    let loan = f
        .apply(
            f.manager,
            "ALL",
            request.version,
            "approve",
            f.approval(request.record_id, InterestMethod::InterestFree),
        )
        .await
        .unwrap()
        .loan_id
        .unwrap();
    let tx = f.db.begin().await.unwrap();
    let page = list_accounts(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        true,
        None,
        None,
        25,
    )
    .await
    .unwrap();
    assert_eq!(page.nodes[0].id, loan);
    assert!(loan_account(
        &tx,
        &f.actor(f.other_user, "SELF", LoanPermission::Read),
        loan
    )
    .await
    .is_err());
    tx.rollback().await.unwrap();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn concurrent_retries_post_once_conflicting_keys_and_overfunding_are_rejected() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let command = LoanCommand::RecordDisbursement {
        loan_id: loan,
        payment: f.payment(1000),
    };
    let (a, b) = tokio::join!(
        f.apply(f.manager, "ALL", 1, "fund", command.clone()),
        f.apply(f.manager, "ALL", 1, "fund", command)
    );
    let a = a.unwrap();
    let b = b.unwrap();
    assert_eq!(a.posting_ids, b.posting_ids);
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(1000)
    );
    assert!(matches!(
        f.apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(1001)
            }
        )
        .await,
        Err(LoanModuleError::IdempotencyConflict)
    ));
    assert!(matches!(
        f.apply(
            f.manager,
            "ALL",
            a.version,
            "too-much",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(9001)
            }
        )
        .await,
        Err(LoanModuleError::FundingCeilingExceeded)
    ));
    assert_eq!(f.account(loan).await.version, a.version);
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn caller_rollback_removes_payment_postings_versions_and_command_receipt() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let tx = f.db.begin().await.unwrap();
    execute_command(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Disburse),
        &CommandMeta {
            idempotency_key: "rollback-fund".into(),
            expected_version: 1,
        },
        &LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(1000),
        },
    )
    .await
    .unwrap();
    tx.rollback().await.unwrap();
    let account = f.account(loan).await;
    assert_eq!(account.principal.parse::<Decimal>().unwrap(), Decimal::ZERO);
    assert_eq!(account.version, 1);
    let retry = f
        .apply(
            f.manager,
            "ALL",
            1,
            "rollback-fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(1000),
            },
        )
        .await
        .unwrap();
    assert_eq!(retry.version, 2);
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn partial_funding_charges_once_and_overpayment_is_held_separately() {
    let f = Fixture::new(100000).await;
    let loan = f
        .loan(InterestMethod::OneTimePercentage {
            rate: Decimal::from(5),
        })
        .await;
    let first = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund-1",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(4000),
            },
        )
        .await
        .unwrap();
    let second = f
        .apply(
            f.manager,
            "ALL",
            first.version,
            "fund-2",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(6000),
            },
        )
        .await
        .unwrap();
    assert_eq!(
        f.account(loan).await.interest.parse::<Decimal>().unwrap(),
        Decimal::from(500)
    );
    let receipt = f
        .apply(
            f.manager,
            "ALL",
            second.version,
            "receipt",
            LoanCommand::RecordReceipt {
                loan_id: loan,
                payment: f.payment(12000),
            },
        )
        .await
        .unwrap();
    assert_eq!(receipt.unapplied_credit, Decimal::from(1500));
    let account = f.account(loan).await;
    assert_eq!(account.state, "CLOSED");
    assert_eq!(account.principal.parse::<Decimal>().unwrap(), Decimal::ZERO);
    assert_eq!(account.interest.parse::<Decimal>().unwrap(), Decimal::ZERO);
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn external_receipt_invalidates_quote_and_fresh_fnf_posts_only_remaining_debt() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::FnfRecovery);
    let input = f.recovery(Uuid::new_v4(), 5000);
    let tx = f.db.begin().await.unwrap();
    let quote = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    tx.commit().await.unwrap();
    assert_eq!(quote.total(), Decimal::from(5000));
    f.apply(
        f.manager,
        "ALL",
        funded.version,
        "receipt",
        LoanCommand::RecordReceipt {
            loan_id: loan,
            payment: f.payment(1000),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    assert!(matches!(
        post_fnf_recoveries(
            &tx,
            &PostRecoveriesInput {
                actor: &actor,
                quote: &quote
            }
        )
        .await,
        Err(LoanModuleError::VersionConflict)
    ));
    tx.rollback().await.unwrap();
    let tx = f.db.begin().await.unwrap();
    let fresh = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    let result = post_fnf_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &fresh,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    assert_eq!(result.total, Decimal::from(5000));
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(4000)
    );
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn empty_employee_revision_is_invalidated_by_a_new_approved_account() {
    let f = Fixture::new(100000).await;
    let actor = f.actor(f.manager, "ALL", LoanPermission::FnfRecovery);
    let tx = f.db.begin().await.unwrap();
    let quote = prepare_recovery_quote(&tx, &actor, &f.recovery(Uuid::new_v4(), 5000))
        .await
        .unwrap();
    tx.commit().await.unwrap();
    assert!(quote.lines().is_empty());
    f.loan(InterestMethod::InterestFree).await;
    let tx = f.db.begin().await.unwrap();
    assert!(matches!(
        validate_recovery_quote(&tx, &actor, &quote).await,
        Err(LoanModuleError::VersionConflict)
    ));
    tx.rollback().await.unwrap();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn concurrent_approvals_cannot_exceed_company_exposure() {
    let f = Fixture::new(10000).await;
    let one = f.request(f.user, "request-1").await;
    let two = f.request(f.other_user, "request-2").await;
    let (a, b) = tokio::join!(
        f.apply(
            f.manager,
            "ALL",
            one.version,
            "approve-1",
            f.approval(one.record_id, InterestMethod::InterestFree)
        ),
        f.apply(
            f.manager,
            "ALL",
            two.version,
            "approve-2",
            f.approval(two.record_id, InterestMethod::InterestFree)
        )
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    let failure = match (a, b) {
        (Err(error), Ok(_)) | (Ok(_), Err(error)) => error,
        _ => panic!("exactly one concurrent approval must fail"),
    };
    assert!(matches!(failure, LoanModuleError::ExposureExceeded));
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn concurrent_distinct_receipts_require_a_fresh_account_version() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    let command = LoanCommand::RecordReceipt {
        loan_id: loan,
        payment: f.payment(1000),
    };
    let (a, b) = tokio::join!(
        f.apply(
            f.manager,
            "ALL",
            funded.version,
            "receipt-1",
            command.clone()
        ),
        f.apply(f.manager, "ALL", funded.version, "receipt-2", command)
    );
    assert_eq!(usize::from(a.is_ok()) + usize::from(b.is_ok()), 1);
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(9000)
    );
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn salary_capacity_and_supplementary_runs_share_one_monthly_recovery_budget() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        funded.version,
        "arrangement",
        LoanCommand::SetDeduction {
            loan_id: loan,
            effective_from: f.today,
            first_due_date: f.today,
            amount: Decimal::from(500),
            recovery: RecoveryMode::Payroll,
            reason: "reviewed current salary arrangement".into(),
        },
    )
    .await
    .unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut input = f.recovery(Uuid::new_v4(), 1000);
    input.source = RecoverySource::Payroll;
    let tx = f.db.begin().await.unwrap();
    let protected = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    assert_eq!(protected.total(), Decimal::ZERO);
    assert_eq!(protected.lines()[0].deferred, Decimal::from(500));
    tx.rollback().await.unwrap();
    input.eligible_net = Decimal::from(2000);
    let tx = f.db.begin().await.unwrap();
    let quote = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    assert_eq!(quote.total(), Decimal::from(500));
    post_payroll_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    input.source_id = Uuid::new_v4();
    let tx = f.db.begin().await.unwrap();
    let supplement = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    assert_eq!(supplement.total(), Decimal::ZERO);
    tx.rollback().await.unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(9500)
    );
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn external_latest_reversal_is_append_only_and_restores_the_exact_balance() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    let receipt = f
        .apply(
            f.manager,
            "ALL",
            funded.version,
            "receipt",
            LoanCommand::RecordReceipt {
                loan_id: loan,
                payment: f.payment(1000),
            },
        )
        .await
        .unwrap();
    let reversed = f
        .apply(
            f.manager,
            "ALL",
            receipt.version,
            "reverse",
            LoanCommand::ReversePosting {
                loan_id: loan,
                posting_id: receipt.posting_ids[0],
                reason: "fixture duplicate receipt evidence".into(),
                reconciliation_reference: "fixture-correction-review".into(),
                review_fingerprint: None,
            },
        )
        .await
        .unwrap();
    assert_eq!(reversed.posting_ids.len(), 1);
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(10000)
    );
    let tx = f.db.begin().await.unwrap();
    let ledger = loan_ledger(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        loan,
        None,
        25,
    )
    .await
    .unwrap();
    assert_eq!(ledger.nodes.len(), 3);
    assert_eq!(
        ledger
            .nodes
            .iter()
            .filter(|p| p.reversal_of == Some(receipt.posting_ids[0]))
            .count(),
        1
    );
    tx.rollback().await.unwrap();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn multiple_loans_and_supplements_cannot_reset_employee_take_home_protection() {
    let f = Fixture::new(100000).await;
    let first = f.loan(InterestMethod::InterestFree).await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund-1",
            LoanCommand::RecordDisbursement {
                loan_id: first,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        funded.version,
        "external-receipt",
        LoanCommand::RecordReceipt {
            loan_id: first,
            payment: f.payment(5000),
        },
    )
    .await
    .unwrap();
    let request = f.request(f.user, "second-request").await;
    let mut approval = f.approval(request.record_id, InterestMethod::InterestFree);
    if let LoanCommand::DecideRequest { terms, .. } = &mut approval {
        terms.as_mut().unwrap().approved_principal = Decimal::from(5000);
    }
    let second = f
        .apply(f.manager, "ALL", request.version, "approve-2", approval)
        .await
        .unwrap()
        .loan_id
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund-2",
        LoanCommand::RecordDisbursement {
            loan_id: second,
            payment: f.payment(5000),
        },
    )
    .await
    .unwrap();
    for (index, loan) in [first, second].into_iter().enumerate() {
        let version = f.account(loan).await.version;
        f.apply(
            f.manager,
            "ALL",
            version,
            &format!("arrange-{index}"),
            LoanCommand::SetDeduction {
                loan_id: loan,
                effective_from: f.today,
                first_due_date: f.today,
                amount: Decimal::from(500),
                recovery: RecoveryMode::Payroll,
                reason: "reviewed two-loan arrangement".into(),
            },
        )
        .await
        .unwrap();
    }
    let actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut input = f.recovery(Uuid::new_v4(), 1500);
    input.source = RecoverySource::Payroll;
    let tx = f.db.begin().await.unwrap();
    let quote = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    assert_eq!(quote.total(), Decimal::from(500));
    assert_eq!(quote.lines()[0].loan_id, first);
    post_payroll_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    input.source_id = Uuid::new_v4();
    let tx = f.db.begin().await.unwrap();
    let supplement = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    assert_eq!(
        supplement.total(),
        Decimal::ZERO,
        "employee monthly budget was reset for the second loan"
    );
    tx.rollback().await.unwrap();
}
#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn reversing_a_tranche_with_an_unposted_fixed_charge_restores_rounding_carry() {
    let f = Fixture::new(100000).await;
    let loan = f
        .loan(InterestMethod::OneTimeFixed {
            amount: Decimal::new(1, 2),
            approved_principal: Decimal::from(10000),
        })
        .await;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "small-tranche",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(250),
            },
        )
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        funded.version,
        "reverse-small",
        LoanCommand::ReversePosting {
            loan_id: loan,
            posting_id: funded.posting_ids[0],
            reason: "fixture invalid funding record".into(),
            reconciliation_reference: "fixture-reviewed-reversal".into(),
            review_fingerprint: None,
        },
    )
    .await
    .unwrap();
    let row =
        f.db.query_one(Statement::from_sql_and_values(
            DbBackend::Postgres,
            "SELECT rounding_carry::text AS carry FROM loan_account WHERE tenant_id=$1 AND id=$2",
            vec![f.tenant.into(), loan.into()],
        ))
        .await
        .unwrap()
        .unwrap();
    let carry: String = row.try_get("", "carry").unwrap();
    assert_eq!(
        carry.parse::<Decimal>().unwrap(),
        Decimal::ZERO,
        "reversed unfunded tranche still has rounding residue"
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn historical_receipt_reversal_preserves_the_issued_payroll_posting() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let funding = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment: f.payment(10000),
            },
        )
        .await
        .unwrap();
    let receipt = f
        .apply(
            f.manager,
            "ALL",
            funding.version,
            "receipt",
            LoanCommand::RecordReceipt {
                loan_id: loan,
                payment: f.payment(1000),
            },
        )
        .await
        .unwrap();
    f.apply(
        f.manager,
        "ALL",
        receipt.version,
        "arrangement",
        LoanCommand::SetDeduction {
            loan_id: loan,
            effective_from: f.today,
            first_due_date: f.today,
            amount: Decimal::from(500),
            recovery: RecoveryMode::Payroll,
            reason: "HR reviewed payroll start".into(),
        },
    )
    .await
    .unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut input = f.recovery(Uuid::new_v4(), 2000);
    input.source = RecoverySource::Payroll;
    let tx = f.db.begin().await.unwrap();
    let quote = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    let posted = post_payroll_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let version = f.account(loan).await.version;
    let tx = f.db.begin().await.unwrap();
    let review = preview_loan_reversal(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Correct),
        loan,
        receipt.posting_ids[0],
    )
    .await
    .unwrap();
    assert_eq!(review.corrected_principal, Decimal::from(9500));
    tx.rollback().await.unwrap();
    let mut command = LoanCommand::ReversePosting {
        loan_id: loan,
        posting_id: receipt.posting_ids[0],
        reason: "HR reviewed invalid historical receipt".into(),
        reconciliation_reference: "HR-correction-evidence".into(),
        review_fingerprint: Some("altered-preview".into()),
    };
    assert!(matches!(
        f.apply(f.manager, "ALL", version, "stale-review", command.clone())
            .await,
        Err(LoanModuleError::VersionConflict)
    ));
    if let LoanCommand::ReversePosting {
        review_fingerprint, ..
    } = &mut command
    {
        *review_fingerprint = Some(review.review_fingerprint);
    }
    let corrected = f
        .apply(
            f.manager,
            "ALL",
            version,
            "reverse-historical",
            command.clone(),
        )
        .await
        .unwrap();
    let replay = f
        .apply(
            f.manager,
            "ALL",
            version,
            "reverse-historical",
            command.clone(),
        )
        .await
        .unwrap();
    assert_eq!(corrected.posting_ids, replay.posting_ids);
    assert!(matches!(
        f.apply(
            f.manager,
            "ALL",
            corrected.version,
            "duplicate-correction",
            command
        )
        .await,
        Err(LoanModuleError::SourceAlreadyPosted)
    ));
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(9500)
    );
    let row = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT p.amount::text AS amount, (SELECT count(*) FROM loan_posting r WHERE r.tenant_id=p.tenant_id AND r.reversal_of=p.id) AS reversed FROM loan_posting p WHERE p.tenant_id=$1 AND p.id=$2", vec![f.tenant.into(), posted.posting_ids[0].into()])).await.unwrap().unwrap();
    assert_eq!(
        row.try_get::<String>("", "amount")
            .unwrap()
            .parse::<Decimal>()
            .unwrap(),
        Decimal::from(500)
    );
    assert_eq!(row.try_get::<i64>("", "reversed").unwrap(), 0);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn historical_receipt_reconciliation_recalculates_interest_and_preserves_settlement_allocations(
) {
    let f = Fixture::new(100000).await;
    let loan = f
        .loan(InterestMethod::ReducingBalance {
            annual_rate: Decimal::from(12),
            convention: InterestConvention::Act365Fixed,
        })
        .await;
    let start = f.today - chrono::Duration::days(30);
    let historic_terms = Uuid::new_v4();
    // Fixture represents an existing approved account. No production backdating or import path.
    f.db.execute(Statement::from_sql_and_values(DbBackend::Postgres, "INSERT INTO loan_terms_version(id,tenant_id,loan_id,policy_version_id,version,effective_from,terms,approved_by,agreement_evidence) SELECT $3,tenant_id,loan_id,policy_version_id,2,$4,terms,approved_by,agreement_evidence FROM loan_terms_version WHERE tenant_id=$1 AND loan_id=$2 AND version=1", vec![f.tenant.into(),loan.into(),historic_terms.into(),start.into()])).await.unwrap();
    f.db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        "UPDATE loan_account SET current_terms_id=$3 WHERE tenant_id=$1 AND id=$2",
        vec![f.tenant.into(), loan.into(), historic_terms.into()],
    ))
    .await
    .unwrap();
    let mut payment = f.payment(10000);
    payment.value_date = start;
    let funded = f
        .apply(
            f.manager,
            "ALL",
            1,
            "fund",
            LoanCommand::RecordDisbursement {
                loan_id: loan,
                payment,
            },
        )
        .await
        .unwrap();
    let mut payment = f.payment(1000);
    payment.value_date = start + chrono::Duration::days(10);
    let receipt = f
        .apply(
            f.manager,
            "ALL",
            funded.version,
            "receipt",
            LoanCommand::RecordReceipt {
                loan_id: loan,
                payment,
            },
        )
        .await
        .unwrap();
    let receipt_posting = *receipt.posting_ids.last().unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::FnfRecovery);
    let tx = f.db.begin().await.unwrap();
    let input = f.recovery(Uuid::new_v4(), 1000);
    let quote = prepare_recovery_quote(&tx, &actor, &input).await.unwrap();
    let posted = post_fnf_recoveries(
        &tx,
        &PostRecoveriesInput {
            actor: &actor,
            quote: &quote,
        },
    )
    .await
    .unwrap();
    tx.commit().await.unwrap();
    let settlement = *posted.posting_ids.last().unwrap();
    let frozen = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT principal_delta::text AS principal,interest_delta::text AS interest FROM loan_posting WHERE tenant_id=$1 AND id=$2", vec![f.tenant.into(),settlement.into()])).await.unwrap().unwrap();
    // With the erroneous receipt removed, earned interest is 10,000 * 12% * 30/365
    // = 98.63. The unchanged 1,000 settlement first clears that interest;
    // the current principal is 9,098.63, while the original source split stays evidence.
    let expected_principal = Decimal::new(909863, 2);
    let expected_interest = Decimal::ZERO;
    let tx = f.db.begin().await.unwrap();
    let review = preview_loan_reversal(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Correct),
        loan,
        receipt_posting,
    )
    .await
    .unwrap();
    assert_eq!(review.corrected_principal, expected_principal);
    assert_eq!(review.corrected_interest, expected_interest);
    assert_eq!(review.preserved_payroll_postings, vec![settlement]);
    tx.rollback().await.unwrap();
    let corrected = f
        .apply(
            f.manager,
            "ALL",
            review.account_version,
            "historical-correction",
            LoanCommand::ReversePosting {
                loan_id: loan,
                posting_id: receipt_posting,
                reason: "reviewed historic receipt error".into(),
                reconciliation_reference: "fixture-interest-review".into(),
                review_fingerprint: Some(review.review_fingerprint),
            },
        )
        .await
        .unwrap();
    assert_eq!(corrected.posting_ids.len(), 2);
    let account = f.account(loan).await;
    assert_eq!(
        account.principal.parse::<Decimal>().unwrap(),
        expected_principal
    );
    assert_eq!(
        account.interest.parse::<Decimal>().unwrap(),
        expected_interest
    );
    let preserved = f.db.query_one(Statement::from_sql_and_values(DbBackend::Postgres, "SELECT principal_delta::text AS principal,interest_delta::text AS interest FROM loan_posting WHERE tenant_id=$1 AND id=$2", vec![f.tenant.into(),settlement.into()])).await.unwrap().unwrap();
    assert_eq!(
        preserved.try_get::<String>("", "principal").unwrap(),
        frozen.try_get::<String>("", "principal").unwrap()
    );
    assert_eq!(
        preserved.try_get::<String>("", "interest").unwrap(),
        frozen.try_get::<String>("", "interest").unwrap()
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn saved_request_is_private_draft_until_explicit_submission() {
    let f = Fixture::new(100000).await;
    let draft = f
        .apply(
            f.user,
            "SELF",
            1,
            "save-draft",
            LoanCommand::SaveRequest {
                request_id: None,
                employee_id: None,
                policy_id: f.policy,
                amount: Decimal::from(10000),
                purpose: "Reviewed draft purpose".into(),
                notes: None,
                preferences: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    assert_eq!(draft.state, "DRAFT");
    let tx = f.db.begin().await.unwrap();
    let page = list_requests(
        &tx,
        &f.actor(f.user, "SELF", LoanPermission::Read),
        true,
        None,
        None,
        25,
    )
    .await
    .unwrap();
    assert_eq!(page.nodes[0].workflow_instance_id, None);
    let queue = list_requests(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Read),
        false,
        None,
        None,
        25,
    )
    .await
    .unwrap();
    assert!(queue.nodes.is_empty());
    tx.rollback().await.unwrap();
    let submitted = f
        .apply(
            f.user,
            "SELF",
            draft.version,
            "submit-draft",
            LoanCommand::SubmitRequest {
                request_id: Some(draft.record_id),
                employee_id: None,
                policy_id: f.policy,
                amount: Decimal::from(10000),
                purpose: "Reviewed draft purpose".into(),
                notes: None,
                preferences: serde_json::json!({}),
            },
        )
        .await
        .unwrap();
    assert_eq!(submitted.record_id, draft.record_id);
    assert_eq!(submitted.state, "SUBMITTED");
    assert_eq!(submitted.version, draft.version + 1);
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn retired_policy_blocks_new_requests_and_preserves_existing_loan_terms() {
    let f = Fixture::new(100000).await;
    let loan = f.loan(InterestMethod::InterestFree).await;
    let policy = f
        .apply(
            f.manager,
            "ALL",
            1,
            "retire",
            LoanCommand::RetirePolicy {
                policy_id: f.policy,
                reason: "New requests use a newly reviewed policy version".into(),
            },
        )
        .await
        .unwrap();
    assert_eq!(policy.state, "RETIRED");
    let tx = f.db.begin().await.unwrap();
    let versions = loan_policy_versions(
        &tx,
        &f.actor(f.manager, "ALL", LoanPermission::Policy),
        None,
        25,
    )
    .await
    .unwrap();
    assert_eq!(versions.nodes[0].status, "RETIRED");
    tx.rollback().await.unwrap();
    let request = f
        .apply(
            f.user,
            "SELF",
            1,
            "after-retirement",
            LoanCommand::SubmitRequest {
                request_id: None,
                employee_id: None,
                policy_id: f.policy,
                amount: Decimal::from(1000),
                purpose: "New request".into(),
                notes: None,
                preferences: serde_json::json!({}),
            },
        )
        .await;
    assert!(matches!(request, Err(LoanModuleError::PolicyUnavailable)));
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund-existing",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(1000),
        },
    )
    .await
    .unwrap();
    assert_eq!(
        f.account(loan).await.principal.parse::<Decimal>().unwrap(),
        Decimal::from(1000)
    );
}

#[tokio::test]
#[ignore = "requires disposable PostgreSQL fixture"]
async fn hr_can_select_a_same_day_first_recovery_without_deducting_unfunded_money() {
    let f = Fixture::new(100000).await;
    let request = f.request(f.user, "request").await;
    let mut approval = f.approval(request.record_id, InterestMethod::InterestFree);
    if let LoanCommand::DecideRequest { first_due_date, .. } = &mut approval {
        *first_due_date = Some(f.today);
    }
    let approved = f
        .apply(
            f.manager,
            "ALL",
            request.version,
            "same-day-approval",
            approval,
        )
        .await
        .unwrap();
    let loan = approved.loan_id.unwrap();
    let actor = f.actor(f.manager, "ALL", LoanPermission::PayrollRecovery);
    let mut input = f.recovery(Uuid::new_v4(), 2000);
    input.source = RecoverySource::Payroll;
    let tx = f.db.begin().await.unwrap();
    assert_eq!(
        prepare_recovery_quote(&tx, &actor, &input)
            .await
            .unwrap()
            .total(),
        Decimal::ZERO
    );
    tx.rollback().await.unwrap();
    f.apply(
        f.manager,
        "ALL",
        1,
        "fund",
        LoanCommand::RecordDisbursement {
            loan_id: loan,
            payment: f.payment(10000),
        },
    )
    .await
    .unwrap();
    let tx = f.db.begin().await.unwrap();
    assert_eq!(
        prepare_recovery_quote(&tx, &actor, &input)
            .await
            .unwrap()
            .total(),
        Decimal::from(500)
    );
    tx.rollback().await.unwrap();
}
