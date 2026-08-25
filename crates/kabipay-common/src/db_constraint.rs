//! Helpers for classifying PostgreSQL constraint failures without parsing
//! driver-formatted error messages.

use sea_orm::{DbErr, RuntimeErr};

/// Returns the PostgreSQL constraint name attached to a SQLx database error.
///
/// Constraint names are deployment-stable identifiers. Callers can map only
/// explicitly known names to public domain errors and leave all other database
/// failures sanitized by [`crate::KabiPayError::Database`].
pub fn constraint_name(error: &DbErr) -> Option<&str> {
    let runtime_error = match error {
        DbErr::Exec(error) | DbErr::Query(error) => error,
        _ => return None,
    };

    match runtime_error {
        RuntimeErr::SqlxError(sea_orm::SqlxError::Database(error)) => error.constraint(),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn non_driver_errors_do_not_claim_a_constraint() {
        assert_eq!(constraint_name(&DbErr::Custom("uq_example".into())), None);
        assert_eq!(
            constraint_name(&DbErr::Exec(RuntimeErr::Internal("uq_example".into()))),
            None
        );
    }
}
