//! Business logic for kabipay-employee.
//!
//! Resolvers call these functions. Services are the only layer that touches SeaORM.

pub mod company_document_service;
pub mod document_file_service;
pub mod document_service;
pub mod directory_service;
pub mod employee_service;
pub mod employment_history_service;
/// Pluggable file backends: `LOCAL` disk, S3/R2/MinIO (`s3_compat`), future Azure
pub mod object_store;
pub mod offboarding_fnf_service;
pub mod onboarding_service;
pub mod org_service;
pub mod profile_extras_service;
pub mod profile_change_service;
pub mod profile_payload_crypto;
pub mod profile_record_service;
pub mod rbac_admin_service;
pub mod separation_service;
