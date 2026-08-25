//! Tenant-schema table models (Liquibase domains 0005–0030).
//! Generated — do not hand-edit; re-run `scripts/generate_db_entities.py`.

pub mod prelude;
pub use prelude::*;

pub mod d0005_auth_rbac;
pub mod d0006_org_hierarchy;
pub mod d0007_employee_core;
pub mod d0008_document_system;
pub mod d0009_custom_fields;
pub mod d0010_time_shift_roster;
pub mod d0011_leave;
pub mod d0012_payroll;
pub mod d0013_tax_statutory;
pub mod d0014_benefits;
pub mod d0015_expense;
pub mod d0016_recruitment;
pub mod d0017_onboarding_offboarding;
pub mod d0018_performance;
pub mod d0019_lms;
pub mod d0020_succession;
pub mod d0021_compensation;
pub mod d0022_assets;
pub mod d0023_grievance;
pub mod d0024_analytics;
pub mod d0025_workflow;
pub mod d0026_integrations;
pub mod d0027_communication_audit;
pub mod d0028_master_data;
pub mod d0029_file_storage;
pub mod d0030_outbox_events;
pub mod d0031_tax_proof;
pub mod d0032_attendance_punch_policy;
pub mod d0033_travel_request;
pub mod d0035_payroll_arrear;
pub mod d0046_user_notification_preference;
pub mod d0050_employee_self_service;
pub mod d0056_company_documents;
pub mod d0059_file_upload_stage;
pub mod d0060_private_file_cleanup;
pub mod d0063_attendance_management;
