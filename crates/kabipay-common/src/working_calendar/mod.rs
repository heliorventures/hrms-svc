//! Dated tenant working calendars shared by attendance, leave and payroll.

mod rules;
mod types;
mod repository;
mod writes;
mod leave_units;

pub use rules::{is_weekly_off, validate_rule};
pub use types::{CalendarDay, LocationVersion, PolicyVersion, WeeklyOffRule, WorkingCalendarSnapshot};
pub use repository::load_calendar;
pub use writes::{audit_calendar, lock_calendar, bump_calendar_revision};
pub use leave_units::decode_leave_units;

#[cfg(test)]
mod tests;
