//! Retry typed transport failures without relaxing database identity checks.
use super::*;
use crate::service_lifecycle::{retry_dependency, DependencyFailure, HealthCheck, LifecycleConfig};

fn retryable_sqlstate(code: &str) -> bool {
    matches!(
        code,
        "08000" | "08001" | "08003" | "08006" | "53300" | "57P01" | "57P02" | "57P03"
    )
}
impl DependencyFailure {
    pub fn from_sqlx(error: &sqlx::Error) -> Self {
        let transient = match error {
            sqlx::Error::Io(e) => matches!(
                e.kind(),
                std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::HostUnreachable
                    | std::io::ErrorKind::NetworkUnreachable
            ),
            sqlx::Error::PoolTimedOut => true,
            sqlx::Error::Database(e) => e.code().is_some_and(|code| retryable_sqlstate(&code)),
            _ => false,
        };
        if transient {
            Self::temporary()
        } else {
            Self::fatal()
        }
    }
    pub fn from_seaorm(error: &sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::Conn(sea_orm::RuntimeErr::SqlxError(e))
            | sea_orm::DbErr::Exec(sea_orm::RuntimeErr::SqlxError(e))
            | sea_orm::DbErr::Query(sea_orm::RuntimeErr::SqlxError(e)) => Self::from_sqlx(e),
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout) => {
                Self::temporary()
            }
            _ => Self::fatal(),
        }
    }
    pub fn from_kabipay(error: &KabiPayError) -> Self {
        match error {
            KabiPayError::Database(e) => Self::from_seaorm(e),
            _ => Self::fatal(),
        }
    }
}

pub async fn connect_validated(
    dsn: &str,
    cache: &TenantDbCache,
    config: &LifecycleConfig,
) -> Result<DatabaseConnection, DependencyFailure> {
    retry_dependency(config, || async {
        // Preserve typed errors; the legacy CLI connector adds diagnostic text.
        if !skip_ops_preflight() {
            let mut connection = sqlx::postgres::PgConnection::connect(dsn)
                .await
                .map_err(|e| DependencyFailure::from_sqlx(&e))?;
            sqlx::query("SET search_path TO kabipay_ops, public")
                .execute(&mut connection)
                .await
                .map_err(|e| DependencyFailure::from_sqlx(&e))?;
            connection
                .close()
                .await
                .map_err(|e| DependencyFailure::from_sqlx(&e))?;
        }
        let db = Database::connect(ops_connect_options(dsn))
            .await
            .map_err(|e| DependencyFailure::from_seaorm(&e))?;
        cache
            .validate_binding_registration(&db)
            .await
            .map_err(|e| DependencyFailure::from_kabipay(&e))?;
        Ok(db)
    })
    .await
}

pub fn database_check(db: DatabaseConnection, cache: TenantDbCache) -> HealthCheck {
    Arc::new(move || {
        let db = db.clone();
        let cache = cache.clone();
        Box::pin(async move {
            db.ping()
                .await
                .map_err(|e| DependencyFailure::from_seaorm(&e))?;
            cache
                .validate_binding_registration(&db)
                .await
                .map_err(|e| DependencyFailure::from_kabipay(&e))
        })
    })
}

#[cfg(test)]
#[path = "availability_tests.rs"]
mod tests;
