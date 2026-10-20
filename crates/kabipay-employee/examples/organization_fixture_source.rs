//! Explicit synthetic fixture process. Runs the production Auth/source handlers.
use axum::{
    body::Bytes, extract::DefaultBodyLimit, http::HeaderMap, routing::post, Extension, Router,
};
use kabipay_common::db::{TenantDbCache, TenantDbConfig};
use sea_orm::{Database, DbBackend, Schema};
use std::{
    net::{Ipv4Addr, SocketAddrV4},
    sync::Arc,
};
fn schema(ops: bool) {
    let schema = Schema::new(DbBackend::Postgres);
    macro_rules! emit {
        ($entity:expr) => {
            println!(
                "{};",
                schema
                    .create_table_from_entity($entity)
                    .to_string(sea_orm::sea_query::PostgresQueryBuilder)
            );
        };
    }
    if ops {
        println!("CREATE SCHEMA IF NOT EXISTS kabipay_ops;");
        emit!(kabipay_db_entities::ops::billing_cycle::Entity);
        emit!(kabipay_db_entities::ops::feature_flag::Entity);
        emit!(kabipay_db_entities::ops::integration_connector::Entity);
        emit!(kabipay_db_entities::ops::invoice::Entity);
        emit!(kabipay_db_entities::ops::module::Entity);
        emit!(kabipay_db_entities::ops::operator_role::Entity);
        emit!(kabipay_db_entities::ops::operator_session::Entity);
        emit!(kabipay_db_entities::ops::operator_user::Entity);
        emit!(kabipay_db_entities::ops::operator_user_role::Entity);
        emit!(kabipay_db_entities::ops::payment::Entity);
        emit!(kabipay_db_entities::ops::tenant::Entity);
        emit!(kabipay_db_entities::ops::tenant_database::Entity);
        emit!(kabipay_db_entities::ops::tenant_subscription::Entity);
        println!("CREATE TABLE kabipay_ops.module_dependency(module_id UUID NOT NULL,depends_on_module_id UUID NOT NULL,PRIMARY KEY(module_id,depends_on_module_id));");
    } else {
        println!("CREATE SCHEMA tenant_fixture; SET search_path=tenant_fixture,public;");
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::user::Entity);
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::role::Entity);
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::permission::Entity);
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::role_permission::Entity);
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::user_role::Entity);
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::user_session::Entity);
        emit!(kabipay_db_entities::tenant::d0005_auth_rbac::permission_scope::Entity);
        emit!(kabipay_db_entities::tenant::d0006_org_hierarchy::department::Entity);
        emit!(kabipay_db_entities::tenant::d0006_org_hierarchy::cost_center::Entity);
        emit!(kabipay_db_entities::tenant::d0006_org_hierarchy::location::Entity);
        emit!(kabipay_db_entities::tenant::d0006_org_hierarchy::designation::Entity);
        emit!(kabipay_db_entities::tenant::d0007_employee_core::employee::Entity);
        emit!(kabipay_db_entities::tenant::d0007_employee_core::employment_history::Entity);
        emit!(kabipay_db_entities::tenant::d0007_employee_core::employee_pan::Entity);
        emit!(kabipay_db_entities::tenant::d0007_employee_core::employee_aadhaar::Entity);
        emit!(kabipay_db_entities::tenant::d0007_employee_core::employee_bank::Entity);
        emit!(kabipay_db_entities::tenant::d0007_employee_core::dependent::Entity);
        emit!(kabipay_db_entities::tenant::d0008_document_system::document_type::Entity);
        emit!(kabipay_db_entities::tenant::d0008_document_system::employee_document::Entity);
        emit!(
            kabipay_db_entities::tenant::d0008_document_system::document_field_definition::Entity
        );
        emit!(kabipay_db_entities::tenant::d0008_document_system::employee_document_field::Entity);
        emit!(kabipay_db_entities::tenant::d0009_custom_fields::custom_field_definition::Entity);
        emit!(kabipay_db_entities::tenant::d0009_custom_fields::custom_field_value::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::holiday_calendar::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::holiday::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::overtime_rule::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::shift::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::employee_shift::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::roster::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::roster_slot::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::shift_swap_request::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::comp_off_balance::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::attendance::Entity);
        emit!(
            kabipay_db_entities::tenant::d0010_time_shift_roster::attendance_regularization::Entity
        );
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::timesheet_week_batch::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::timesheet_project_assignment::Entity);
        emit!(kabipay_db_entities::tenant::d0010_time_shift_roster::timesheet_entry::Entity);
        emit!(kabipay_db_entities::tenant::d0011_leave::leave_type::Entity);
        emit!(kabipay_db_entities::tenant::d0011_leave::leave_policy::Entity);
        emit!(kabipay_db_entities::tenant::d0011_leave::leave_balance::Entity);
        emit!(kabipay_db_entities::tenant::d0011_leave::leave_accrual_log::Entity);
        emit!(kabipay_db_entities::tenant::d0011_leave::leave_request::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::salary_component::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::salary_structure::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::salary_structure_component::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::employee_salary_structure::Entity);
        emit!(
            kabipay_db_entities::tenant::d0012_payroll::employee_salary_component_override::Entity
        );
        emit!(kabipay_db_entities::tenant::d0012_payroll::payroll_cycle::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::payslip::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::payslip_component::Entity);
        emit!(kabipay_db_entities::tenant::d0012_payroll::payroll_compliance_setting::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::tax_configuration_version::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::tax_slab::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::tax_computation::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::statutory_filing::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::tax_section_definition::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::form_16::Entity);
        emit!(kabipay_db_entities::tenant::d0013_tax_statutory::labour_law_register::Entity);
        emit!(kabipay_db_entities::tenant::d0014_benefits::benefit_type::Entity);
        emit!(kabipay_db_entities::tenant::d0014_benefits::benefit_plan::Entity);
        emit!(kabipay_db_entities::tenant::d0014_benefits::employee_benefit_enrollment::Entity);
        emit!(kabipay_db_entities::tenant::d0014_benefits::benefit_claim::Entity);
        emit!(kabipay_db_entities::tenant::d0015_expense::expense_category::Entity);
        emit!(kabipay_db_entities::tenant::d0015_expense::expense_policy::Entity);
        emit!(kabipay_db_entities::tenant::d0015_expense::expense::Entity);
        emit!(kabipay_db_entities::tenant::d0015_expense::expense_item::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::hiring_stage::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::job_posting::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::application::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::application_stage_log::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::interview::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::interview_scorecard::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::referral::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::offer_letter::Entity);
        emit!(kabipay_db_entities::tenant::d0016_recruitment::job_board_sync::Entity);
        emit!(
            kabipay_db_entities::tenant::d0017_onboarding_offboarding::onboarding_checklist::Entity
        );
        emit!(kabipay_db_entities::tenant::d0017_onboarding_offboarding::separation::Entity);
        emit!(kabipay_db_entities::tenant::d0017_onboarding_offboarding::exit_interview::Entity);
        emit!(kabipay_db_entities::tenant::d0017_onboarding_offboarding::fnf_settlement::Entity);
        emit!(
            kabipay_db_entities::tenant::d0017_onboarding_offboarding::clearance_checklist::Entity
        );
        emit!(kabipay_db_entities::tenant::d0018_performance::review_cycle::Entity);
        emit!(kabipay_db_entities::tenant::d0018_performance::goal::Entity);
        emit!(kabipay_db_entities::tenant::d0018_performance::kpi::Entity);
        emit!(kabipay_db_entities::tenant::d0018_performance::feedback_response::Entity);
        emit!(kabipay_db_entities::tenant::d0018_performance::performance_rating::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::skill::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::employee_skill::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::course::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::course_module::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::learning_path::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::learning_path_course::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::enrollment::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::course_progress::Entity);
        emit!(kabipay_db_entities::tenant::d0019_lms::certification::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::competency::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::competency_level::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::designation_competency::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::employee_competency::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::talent_pool::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::talent_pool_member::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::succession_plan::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::succession_candidate::Entity);
        emit!(kabipay_db_entities::tenant::d0020_succession::career_path::Entity);
        emit!(kabipay_db_entities::tenant::d0021_compensation::salary_band::Entity);
        emit!(kabipay_db_entities::tenant::d0021_compensation::compensation_review_cycle::Entity);
        emit!(kabipay_db_entities::tenant::d0021_compensation::compensation_review_item::Entity);
        emit!(kabipay_db_entities::tenant::d0021_compensation::bonus_plan::Entity);
        emit!(kabipay_db_entities::tenant::d0021_compensation::bonus_payout::Entity);
        emit!(kabipay_db_entities::tenant::d0021_compensation::equity_grant::Entity);
        emit!(kabipay_db_entities::tenant::d0022_assets::asset_category::Entity);
        emit!(kabipay_db_entities::tenant::d0022_assets::asset::Entity);
        emit!(kabipay_db_entities::tenant::d0022_assets::asset_allocation::Entity);
        emit!(kabipay_db_entities::tenant::d0022_assets::asset_return_log::Entity);
        emit!(kabipay_db_entities::tenant::d0023_grievance::grievance_category::Entity);
        emit!(kabipay_db_entities::tenant::d0023_grievance::grievance_case::Entity);
        emit!(kabipay_db_entities::tenant::d0023_grievance::case_participant::Entity);
        emit!(kabipay_db_entities::tenant::d0023_grievance::case_action::Entity);
        emit!(kabipay_db_entities::tenant::d0023_grievance::disciplinary_action::Entity);
        emit!(kabipay_db_entities::tenant::d0024_analytics::report_definition::Entity);
        emit!(kabipay_db_entities::tenant::d0024_analytics::report_schedule::Entity);
        emit!(kabipay_db_entities::tenant::d0024_analytics::dashboard::Entity);
        emit!(kabipay_db_entities::tenant::d0024_analytics::dashboard_widget::Entity);
        emit!(kabipay_db_entities::tenant::d0024_analytics::workforce_snapshot::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::workflow::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::workflow_step::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::workflow_instance::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::workflow_action::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::approval_matrix::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::approval_rule::Entity);
        emit!(kabipay_db_entities::tenant::d0025_workflow::approval_condition::Entity);
        emit!(kabipay_db_entities::tenant::d0026_integrations::tenant_integration::Entity);
        emit!(kabipay_db_entities::tenant::d0026_integrations::webhook_subscription::Entity);
        emit!(kabipay_db_entities::tenant::d0027_communication_audit::announcement::Entity);
        emit!(kabipay_db_entities::tenant::d0027_communication_audit::notification::Entity);
        emit!(kabipay_db_entities::tenant::d0028_master_data::master_data::Entity);
        emit!(kabipay_db_entities::tenant::d0029_file_storage::file_storage::Entity);
        emit!(kabipay_db_entities::tenant::d0031_tax_proof::tax_proof_line::Entity);
        emit!(kabipay_db_entities::tenant::d0032_attendance_punch_policy::attendance_punch_policy::Entity);
        emit!(kabipay_db_entities::tenant::d0033_travel_request::travel_request::Entity);
        emit!(kabipay_db_entities::tenant::d0035_payroll_arrear::payroll_arrear::Entity);
        emit!(kabipay_db_entities::tenant::d0046_user_notification_preference::user_notification_preference::Entity);
        emit!(kabipay_db_entities::tenant::d0050_employee_self_service::employee_education::Entity);
        emit!(kabipay_db_entities::tenant::d0050_employee_self_service::employee_work_experience::Entity);
        emit!(kabipay_db_entities::tenant::d0050_employee_self_service::employee_profile_change_request::Entity);
        emit!(kabipay_db_entities::tenant::d0050_employee_self_service::employee_education_document::Entity);
        emit!(kabipay_db_entities::tenant::d0050_employee_self_service::employee_work_experience_document::Entity);
        emit!(kabipay_db_entities::tenant::d0056_company_documents::company_document::Entity);
        emit!(kabipay_db_entities::tenant::d0059_file_upload_stage::file_upload_stage::Entity);
        emit!(kabipay_db_entities::tenant::d0060_private_file_cleanup::private_file_cleanup_task::Entity);
        emit!(kabipay_db_entities::tenant::d0063_attendance_management::attendance_adjustment_audit::Entity);
        emit!(kabipay_db_entities::tenant::d0074_automated_employee_notifications::notification_automation_setting::Entity);
        emit!(kabipay_db_entities::tenant::d0074_automated_employee_notifications::employee_celebration_preference::Entity);
        emit!(kabipay_db_entities::tenant::d0074_automated_employee_notifications::automated_notification_occurrence::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::performance_program::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::appraisal_template::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::appraisal_template_section::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::appraisal_question::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::appraisal_question_option::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::performance_participant::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::appraisal_answer::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::continuous_feedback::Entity);
        emit!(kabipay_db_entities::tenant::d0075_performance_appraisal_lifecycle::performance_admin_exception::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_audience_department::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_section::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_question::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_question_option::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_assignment::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_response::Entity);
        emit!(kabipay_db_entities::tenant::d0076_anonymous_surveys::survey_answer::Entity);
        emit!(kabipay_db_entities::tenant::d0077_unpaid_leave_payroll::payroll_unpaid_leave_allocation::Entity);
        emit!(kabipay_db_entities::tenant::d0077_unpaid_leave_payroll::payroll_unpaid_leave_policy::Entity);
        emit!(
            kabipay_db_entities::tenant::d0077_unpaid_leave_payroll::payslip_unpaid_leave::Entity
        );
        emit!(kabipay_db_entities::tenant::d0078_comp_off::comp_off_policy::Entity);
        emit!(kabipay_db_entities::tenant::d0078_comp_off::comp_off_claim::Entity);
        emit!(kabipay_db_entities::tenant::d0078_comp_off::comp_off_credit::Entity);
        emit!(kabipay_db_entities::tenant::d0078_comp_off::comp_off_allocation::Entity);
        emit!(
            kabipay_db_entities::tenant::d0079_announcement_video::announcement_video_stage::Entity
        );
        emit!(kabipay_db_entities::tenant::d0080_prejoining::prejoining_config::Entity);
        emit!(kabipay_db_entities::tenant::d0080_prejoining::prejoining_candidate::Entity);
        emit!(kabipay_db_entities::tenant::d0080_prejoining::prejoining_event::Entity);
        emit!(kabipay_db_entities::tenant::d0080_prejoining::prejoining_document::Entity);
        emit!(kabipay_db_entities::tenant::d0084_survey_targeting_corrections::survey_audience_location::Entity);
        emit!(kabipay_db_entities::tenant::d0084_survey_targeting_corrections::survey_audience_employee::Entity);
        emit!(kabipay_db_entities::tenant::d0084_survey_targeting_corrections::survey_revision::Entity);
        emit!(kabipay_db_entities::tenant::d0084_survey_targeting_corrections::survey_audience_scope::Entity);
        emit!(kabipay_db_entities::tenant::d0087_attendance_day_boundary::attendance_day_profile::Entity);
        emit!(kabipay_db_entities::tenant::d0087_attendance_day_boundary::attendance_day_policy_version::Entity);
        emit!(kabipay_db_entities::tenant::d0087_attendance_day_boundary::attendance_day_window::Entity);
        emit!(kabipay_db_entities::tenant::d0089_user_guidance_state::user_guidance_state::Entity);
        emit!(kabipay_db_entities::tenant::d0090_payroll_period_configuration::payroll_period_input::Entity);
        emit!(kabipay_db_entities::tenant::d0090_payroll_period_configuration::employee_payroll_rule::Entity);
        emit!(kabipay_db_entities::tenant::d0090_payroll_period_configuration::payslip_statement::Entity);
        emit!(kabipay_db_entities::tenant::d0090_payroll_period_configuration::payroll_period_adjustment::Entity);
        emit!(
            kabipay_db_entities::tenant::d0091_leave_import_history::leave_import_history::Entity
        );
        emit!(kabipay_db_entities::tenant::d0092_tenant_import_tracking::tenant_import_run::Entity);
        emit!(
            kabipay_db_entities::tenant::d0092_tenant_import_tracking::tenant_import_record::Entity
        );
        emit!(kabipay_db_entities::tenant::d0093_tax_projection_configuration::employee_tax_settings::Entity);
        emit!(kabipay_db_entities::tenant::d0093_tax_projection_configuration::company_payroll_rule::Entity);
        emit!(kabipay_db_entities::tenant::d0093_tax_projection_configuration::employee_tax_declaration::Entity);
        emit!(kabipay_db_entities::tenant::d0093_tax_projection_configuration::employee_tax_history::Entity);
        emit!(kabipay_db_entities::tenant::d0094_payroll_draft_calculation::payroll_draft_calculation::Entity);
        emit!(kabipay_db_entities::tenant::d0097_location_working_calendar::working_calendar_profile::Entity);
        emit!(kabipay_db_entities::tenant::d0097_location_working_calendar::employee_location_assignment::Entity);
        emit!(kabipay_db_entities::tenant::d0097_location_working_calendar::weekly_off_policy_version::Entity);
        emit!(kabipay_db_entities::tenant::d0098_leave_working_dates::leave_working_date_snapshot::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_policy_version::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_request::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_account::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_terms_version::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_schedule_version::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_schedule_item::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_period_override::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_disbursement::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_receipt::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_posting::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_ledger_entry::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_allocation::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_unapplied_credit::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_employee_state::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_command_receipt::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_audit_event::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_internal_outbox::Entity);
        emit!(kabipay_db_entities::tenant::d0103_employee_loans::loan_consumer_receipt::Entity);
        emit!(
            kabipay_db_entities::tenant::d0105_payslip_loan_evidence::payslip_loan_snapshot::Entity
        );
        emit!(kabipay_db_entities::tenant::d0106_loan_exit_coordination::employee_exit_operation::Entity);
        emit!(kabipay_db_entities::tenant::d0106_loan_exit_coordination::loan_exit_review::Entity);
        emit!(kabipay_db_entities::tenant::d0106_loan_exit_coordination::loan_exit_approval_guard::Entity);
        emit!(
            kabipay_db_entities::tenant::d0106_loan_exit_coordination::fnf_financial_review::Entity
        );
        emit!(kabipay_db_entities::tenant::d0106_loan_exit_coordination::fnf_payout_source::Entity);
        emit!(kabipay_db_entities::tenant::d0106_loan_exit_coordination::loan_exit_arrangement::Entity);
        emit!(
            kabipay_db_entities::tenant::d0107_loan_reconciliation::loan_reconciliation_run::Entity
        );
        emit!(kabipay_db_entities::tenant::d0107_loan_reconciliation::loan_reconciliation_status::Entity);
        println!(
            r#"DO $fixture$ DECLARE row RECORD; BEGIN FOR row IN SELECT table_name FROM information_schema.columns WHERE table_schema='tenant_fixture' GROUP BY table_name HAVING count(*) FILTER(WHERE column_name IN ('tenant_id','id'))=2 LOOP EXECUTE format('ALTER TABLE tenant_fixture.%I ADD UNIQUE(tenant_id,id)',row.table_name); END LOOP; END $fixture$;
CREATE TABLE payroll_period_input_audit(id UUID PRIMARY KEY DEFAULT gen_random_uuid(),tenant_id UUID NOT NULL,period_input_id UUID NOT NULL,revision INT NOT NULL,input JSONB NOT NULL,changed_by UUID NOT NULL,created_at TIMESTAMPTZ NOT NULL DEFAULT now(),UNIQUE(period_input_id,revision),FOREIGN KEY(tenant_id,changed_by) REFERENCES "user"(tenant_id,id),FOREIGN KEY(tenant_id,period_input_id) REFERENCES payroll_period_input(tenant_id,id));
"#
        );
        println!(
            r#"ALTER TABLE loan_posting DROP COLUMN IF EXISTS actor_principal_id, ALTER COLUMN actor_id SET NOT NULL;
ALTER TABLE loan_audit_event DROP COLUMN IF EXISTS actor_principal_id, ALTER COLUMN actor_id SET NOT NULL;
ALTER TABLE payroll_period_input DROP COLUMN IF EXISTS updated_by_principal_id, ALTER COLUMN updated_by SET NOT NULL;
ALTER TABLE payroll_period_input_audit DROP COLUMN IF EXISTS changed_by_principal_id, ALTER COLUMN changed_by SET NOT NULL;
ALTER TABLE payroll_period_adjustment DROP COLUMN IF EXISTS updated_by_principal_id, ALTER COLUMN updated_by SET NOT NULL;
ALTER TABLE payroll_draft_calculation DROP COLUMN IF EXISTS calculated_by_principal_id,DROP COLUMN IF EXISTS finalized_by_principal_id,ALTER COLUMN calculated_by SET NOT NULL;
ALTER TABLE payroll_cycle DROP COLUMN IF EXISTS processed_by_principal_id;
ALTER TABLE continuous_feedback ALTER COLUMN created_by_user_id SET NOT NULL;"#
        );
    }
}
fn guarded_url(name: &str, databases: &[&str]) -> anyhow::Result<String> {
    let value = std::env::var(name)?;
    let url = reqwest::Url::parse(&value)?;
    anyhow::ensure!(
        url.host_str() == Some("127.0.0.1")
            && databases.contains(&url.path().trim_start_matches('/')),
        "fixture refuses non-synthetic database"
    );
    Ok(value)
}
#[tokio::main]
async fn main() -> anyhow::Result<()> {
    if std::env::args().any(|a| a == "--print-source-schema") {
        schema(false);
        return Ok(());
    }
    if std::env::args().any(|a| a == "--print-ops-schema") {
        schema(true);
        return Ok(());
    }
    anyhow::ensure!(
        std::env::var("KABIPAY_ORGANIZATION_FIXTURE").as_deref() == Ok("1"),
        "explicit synthetic opt in required"
    );
    guarded_url(
        "KABIPAY_BOUND_DATABASE_URL",
        &[
            "organization_company_a_fixture",
            "organization_company_b_fixture",
        ],
    )?;
    let ops = Database::connect(guarded_url(
        "KABIPAY_FIXTURE_OPS_DATABASE_URL",
        &["org_execution_fixture"],
    )?)
    .await?;
    let cache = TenantDbCache::from_env()?;
    let fallback = TenantDbConfig {
        db_host: "127.0.0.1".into(),
        db_port: 1,
        db_name: "unused".into(),
        db_user: "unused".into(),
        db_password: "unused".into(),
        schema_name: "tenant_fixture".into(),
    };
    let state = kabipay_auth::state::AppState {
        ops_db: ops.clone(),
        tenant_cache: cache.clone(),
        tenant_fallback: fallback.clone(),
        jwt: kabipay_auth::jwt::JwtConfig::from_env(),
    };
    let auth = Router::new()
        .route(
            "/auth/client/organization-identity",
            post(kabipay_auth::organization_identity::organization_identity),
        )
        .route(
            "/auth/internal/account-eligibility",
            post(kabipay_auth::account_eligibility::account_eligibility),
        )
        .route(
            "/auth/internal/employment-eligibility",
            post(kabipay_auth::account_eligibility::employment_eligibility),
        )
        .layer(Extension(Arc::new(
            kabipay_organization_security::service_verifier_from_env("kabipay-auth")?,
        )))
        .with_state(state);
    let service = std::env::var("KABIPAY_FIXTURE_SERVICE")?;
    anyhow::ensure!(
        matches!(
            service.as_str(),
            "employee" | "leave" | "payroll" | "workflow"
        ),
        "unknown fixture source service"
    );
    if service == "employee" {
        let (cache, ops, fallback) = (cache.clone(), ops.clone(), fallback.clone());
        tokio::spawn(async move {
            let mut tick = tokio::time::interval(std::time::Duration::from_millis(250));
            loop {
                tick.tick().await;
                if let Ok((tenant, db)) =
                    kabipay_organization_tenant::fixed_database(&cache, &ops, &fallback).await
                {
                    let _ = kabipay_organization_tenant::bootstrap_once(&db, tenant, 100).await;
                }
            }
        });
    }
    let worker = if service == "payroll" {
        let (cache, ops, fallback) = (cache.clone(), ops.clone(), fallback.clone());
        Router::new().route(
            "/fixture/run-one",
            post(move || {
                let (cache, ops, fallback) = (cache.clone(), ops.clone(), fallback.clone());
                async move {
                    let (tenant, db) =
                        kabipay_organization_tenant::fixed_database(&cache, &ops, &fallback)
                            .await
                            .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
                    let worked =
                        kabipay_payroll::organization_job_worker::run_one(&db, &ops, tenant)
                            .await
                            .map_err(|_| axum::http::StatusCode::SERVICE_UNAVAILABLE)?;
                    Ok::<_, axum::http::StatusCode>(axum::Json(
                        serde_json::json!({"worked":worked}),
                    ))
                }
            }),
        )
    } else {
        Router::new()
    };
    let command = Router::new().route(
        "/internal/organization/command",
        post(move |headers: HeaderMap, body: Bytes| {
            let (cache, ops, fallback, service) = (
                cache.clone(),
                ops.clone(),
                fallback.clone(),
                service.clone(),
            );
            async move {
                let (tenant, db) =
                    kabipay_organization_tenant::fixed_database(&cache, &ops, &fallback)
                        .await
                        .map_err(|_| {
                            (
                                axum::http::StatusCode::SERVICE_UNAVAILABLE,
                                "fixture binding unavailable".to_owned(),
                            )
                        })?;
                if service == "employee" {
                    kabipay_organization_tenant::handle(
                        kabipay_employee::organization::EmployeeAdapter,
                        tenant,
                        db,
                        &ops,
                        headers,
                        body,
                    )
                    .await
                } else if service == "leave" {
                    kabipay_organization_tenant::handle(
                        kabipay_leave::organization::LeaveAdapter,
                        tenant,
                        db,
                        &ops,
                        headers,
                        body,
                    )
                    .await
                } else if service == "payroll" {
                    kabipay_organization_tenant::handle(
                        kabipay_payroll::organization::PayrollAdapter,
                        tenant,
                        db,
                        &ops,
                        headers,
                        body,
                    )
                    .await
                } else {
                    kabipay_organization_tenant::handle(
                        kabipay_workflow::organization::WorkflowAdapter,
                        tenant,
                        db,
                        &ops,
                        headers,
                        body,
                    )
                    .await
                }
            }
        }),
    );
    let port = std::env::var("KABIPAY_FIXTURE_PORT")?.parse::<u16>()?;
    let listener =
        tokio::net::TcpListener::bind(SocketAddrV4::new(Ipv4Addr::LOCALHOST, port)).await?;
    println!("organization_fixture_source_ready:{port}");
    axum::serve(
        listener,
        auth.merge(command)
            .merge(worker)
            .layer(DefaultBodyLimit::max(65536)),
    )
    .await?;
    Ok(())
}
