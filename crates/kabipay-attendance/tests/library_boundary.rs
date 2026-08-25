//! Integration-level contract for the attendance crate boundary.
//!
//! This test fails if the production resolver/service graph is only declared
//! by the `kabipay-attendance` binary and cannot be imported by consumers.

use kabipay_attendance::{attendance_management::AttendancePage, MutationRoot, QueryRoot};

#[test]
fn attendance_public_api_exposes_only_the_required_library_boundary_types() {
    let _ = std::any::TypeId::of::<QueryRoot>();
    let _ = std::any::TypeId::of::<MutationRoot>();
    let _ = std::any::TypeId::of::<AttendancePage<()>>();
}
