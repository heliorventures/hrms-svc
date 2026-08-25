//! Attendance domain library used by the subgraph binary and integration tests.

mod resolvers;
mod services;

pub use resolvers::{MutationRoot, QueryRoot};

/// Public attendance-management types supported for integration consumers.
///
/// Keep this facade intentionally narrow. Task 4B may add only the
/// regularization-specific types or functions its integration tests require.
pub mod attendance_management {
    pub use crate::services::attendance_management_service::AttendancePage;
    pub use crate::services::attendance_regularization_service::{
        create_managed_attendance_segment_in_transaction, ManagedCreateCommand, SegmentTimes,
    };
}
