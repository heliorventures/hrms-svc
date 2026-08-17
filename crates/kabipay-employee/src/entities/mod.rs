#![allow(unused_imports)] // re-exports for resolvers / services

//! Org hierarchy, employee core, documents, custom fields, onboarding/offboarding (0006–0009, 0017).
pub use kabipay_db_entities::tenant::{
    d0006_org_hierarchy, d0007_employee_core, d0008_document_system, d0009_custom_fields,
    d0017_onboarding_offboarding, d0029_file_storage, d0050_employee_self_service,
    d0056_company_documents,
};
