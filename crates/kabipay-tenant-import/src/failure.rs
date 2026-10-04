//! Failure evidence excludes SQL text, bind values, error detail and connection strings.
use sea_orm::{DbErr, RuntimeErr, SqlxError, SqlxPostgresError};
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Failure {
    pub code: String,
    pub commit_status: &'static str,
    pub sql_state: Option<String>,
    pub constraint: Option<String>,
    pub table: Option<String>,
}

fn identifier(value: Option<&str>) -> Option<String> {
    value
        .filter(|value| crate::preview::safe_identifier(value))
        .map(String::from)
}

pub fn describe(error: &anyhow::Error) -> Failure {
    let mut failure = Failure {
        code: crate::cli::safe_error(error),
        // A network failure around COMMIT requires persisted-run reconciliation, not an assumed rollback.
        commit_status: "UNCONFIRMED_CHECK_PERSISTED_RUN",
        sql_state: None,
        constraint: None,
        table: None,
    };
    for cause in error.chain() {
        if let Some(
            DbErr::Exec(RuntimeErr::SqlxError(SqlxError::Database(db)))
            | DbErr::Query(RuntimeErr::SqlxError(SqlxError::Database(db)))
            | DbErr::Conn(RuntimeErr::SqlxError(SqlxError::Database(db))),
        ) = cause.downcast_ref::<DbErr>()
        {
            failure.sql_state = db
                .code()
                .filter(|code| {
                    code.len() == 5 && code.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .map(|code| code.into_owned());
            if let Some(pg) = db.try_downcast_ref::<SqlxPostgresError>() {
                failure.constraint = identifier(pg.constraint());
                failure.table = identifier(pg.table());
            }
        }
    }
    failure
}
