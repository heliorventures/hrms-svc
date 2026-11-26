//! Recognize configuration faults through typed SQLSTATE, never error messages.
use kabipay_common::KabiPayError;
use sea_orm::{DbErr, RuntimeErr};

fn permanent_sqlstate(code: &str) -> bool {
    matches!(
        code,
        "42501" | "42P01" | "42703" | "3F000" | "28000" | "28P01" | "3D000"
    )
}

pub fn permanent_database_fault(error: &KabiPayError) -> bool {
    let KabiPayError::Database(error) = error else {
        return false;
    };
    match error {
        DbErr::Conn(RuntimeErr::SqlxError(error))
        | DbErr::Exec(RuntimeErr::SqlxError(error))
        | DbErr::Query(RuntimeErr::SqlxError(error)) => error
            .as_database_error()
            .and_then(|error| error.code())
            .is_some_and(|code| permanent_sqlstate(&code)),
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn permissions_and_schema_faults_hold_but_transactions_and_transport_retry() {
        for code in [
            "42501", "42P01", "42703", "3F000", "28000", "28P01", "3D000",
        ] {
            assert!(permanent_sqlstate(code));
        }
        for code in [
            "40001", "40P01", "55P03", "08006", "57P01", "53300", "23505", "unknown",
        ] {
            assert!(!permanent_sqlstate(code));
        }
        assert!(!permanent_database_fault(&KabiPayError::Validation(
            "settings changed".into()
        )));
        assert!(!permanent_database_fault(&KabiPayError::Database(
            DbErr::Custom("permission denied 42501".into())
        )));
    }
}
