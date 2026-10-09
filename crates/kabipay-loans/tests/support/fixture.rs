// Runs only against the disposable database created by scripts/test-loans.ps1.
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

pub struct Fixture {
    pub db: DatabaseConnection,
    pub tenant: Uuid,
    pub employee: Uuid,
    pub user: Uuid,
    pub other: Uuid,
    pub other_user: Uuid,
    pub manager: Uuid,
    pub workflow: Uuid,
    pub step: Uuid,
    pub policy: Uuid,
    pub today: NaiveDate,
}
pub async fn sql(db: &DatabaseConnection, source: &str, values: Vec<Value>) {
    db.execute(Statement::from_sql_and_values(
        DbBackend::Postgres,
        source,
        values,
    ))
    .await
    .unwrap();
}
impl Fixture {
    pub async fn new(company_limit: i64) -> Self {
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
    pub fn currency(&self) -> Currency {
        Currency {
            code: "INR".into(),
            minor_units: 2,
        }
    }
    pub fn claims(&self, user: Uuid, scope: &str, permission: LoanPermission) -> ClientClaims {
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
    pub fn actor(&self, user: Uuid, scope: &str, permission: LoanPermission) -> LoanActorScope {
        LoanActorScope::from_verified_claims(
            &self.claims(user, scope, permission),
            self.tenant,
            permission,
        )
        .unwrap()
    }
    pub async fn apply(
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
    pub async fn request(&self, user: Uuid, key: &str) -> LoanCommandResult {
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
    pub fn terms(&self, interest: InterestMethod) -> LoanTerms {
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
    pub fn approval(&self, request: Uuid, interest: InterestMethod) -> LoanCommand {
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
    pub async fn loan(&self, interest: InterestMethod) -> Uuid {
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
    pub fn payment(&self, amount: i64) -> RecordedPayment {
        RecordedPayment {
            amount: Decimal::from(amount),
            value_date: self.today,
            method: "EXTERNAL_BANK".into(),
            external_reference: format!("fixture-{amount}"),
            evidence_reference: "fixture-bank-evidence".into(),
        }
    }
    pub async fn account(&self, loan: Uuid) -> LoanAccountView {
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
    pub fn recovery(&self, source: Uuid, amount: i64) -> RecoveryInput {
        RecoveryInput {
            employee_id: self.employee,
            source: RecoverySource::Fnf,
            source_id: source,
            source_revision: 1,
            period_start: self.today.with_day(1).unwrap(),
            value_date: self.today,
            eligible_net: Decimal::from(amount),
            available_net: Decimal::from(amount),
            currency: self.currency(),
        }
    }
}
use chrono::Datelike;
