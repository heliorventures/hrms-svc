//! Canonical error type for the KabiPay backend.
//!
//! Every service uses this `KabiPayError`. Mapping to GraphQL error codes lives here
//! so that frontend clients can switch on a stable `code` rather than message strings.

use uuid::Uuid;

/// Convenience result alias.
pub type KabiPayResult<T> = Result<T, KabiPayError>;

/// Canonical error type returned from services, resolvers, and middleware.
///
/// Variants map to stable GraphQL error codes via [`KabiPayError::into_graphql`].
#[derive(Debug, thiserror::Error)]
pub enum KabiPayError {
    #[error("not found: {entity} with id {id}")]
    NotFound { entity: &'static str, id: String },

    #[error("tenant not found: {0}")]
    TenantNotFound(String),

    #[error("seat limit reached for module {module_code} — contracted: {contracted}, current: {current}")]
    SeatLimitReached {
        module_code: String,
        contracted: i32,
        current: i32,
    },

    #[error("module {0} is not subscribed for this tenant")]
    ModuleNotSubscribed(String),

    #[error("tenant is suspended: {0}")]
    TenantSuspended(Uuid),

    #[error("unauthorised — invalid or missing token")]
    Unauthorised,

    #[error("forbidden — insufficient permissions: {0}")]
    Forbidden(String),

    #[error("validation error: {0}")]
    Validation(String),

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("database error: {0}")]
    Database(#[from] sea_orm::DbErr),

    #[error("organization workspace is temporarily unavailable: {0}")]
    TenantDatabaseUnavailable(Uuid),

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("internal error: {0}")]
    Internal(String),
}

impl KabiPayError {
    /// Classify tenant-schema database errors without exposing transient pool
    /// failures as generic internal errors to callers.
    pub fn from_tenant_db(tenant_id: Uuid, error: sea_orm::DbErr) -> Self {
        match error {
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout) => {
                Self::TenantDatabaseUnavailable(tenant_id)
            }
            other => Self::Database(other),
        }
    }

    /// Stable error code exposed in GraphQL responses.
    /// Frontend clients MUST switch on this, never on the message.
    pub fn code(&self) -> &'static str {
        match self {
            Self::NotFound { .. } => "NOT_FOUND",
            Self::TenantNotFound(_) => "TENANT_NOT_FOUND",
            Self::SeatLimitReached { .. } => "SEAT_LIMIT_REACHED",
            Self::ModuleNotSubscribed(_) => "MODULE_NOT_SUBSCRIBED",
            Self::TenantSuspended(_) => "TENANT_SUSPENDED",
            Self::Unauthorised => "UNAUTHENTICATED",
            Self::Forbidden(_) => "FORBIDDEN",
            Self::Validation(_) => "VALIDATION_ERROR",
            Self::Conflict(_) => "CONFLICT",
            Self::Database(_) => "DATABASE_ERROR",
            Self::TenantDatabaseUnavailable(_) => "TENANT_DATABASE_UNAVAILABLE",
            Self::Jwt(_) => "UNAUTHENTICATED",
            Self::Json(_) => "INVALID_JSON",
            Self::Internal(_) => "INTERNAL_ERROR",
        }
    }

    /// HTTP status code for REST-style responses (gateway, auth endpoints).
    pub fn http_status(&self) -> axum::http::StatusCode {
        use axum::http::StatusCode as S;
        match self {
            Self::NotFound { .. } | Self::TenantNotFound(_) => S::NOT_FOUND,
            Self::SeatLimitReached { .. }
            | Self::ModuleNotSubscribed(_)
            | Self::TenantSuspended(_) => S::FORBIDDEN,
            Self::Unauthorised | Self::Jwt(_) => S::UNAUTHORIZED,
            Self::Forbidden(_) => S::FORBIDDEN,
            Self::Validation(_) | Self::Json(_) => S::BAD_REQUEST,
            Self::Conflict(_) => S::CONFLICT,
            Self::TenantDatabaseUnavailable(_) => S::SERVICE_UNAVAILABLE,
            Self::Database(_) | Self::Internal(_) => S::INTERNAL_SERVER_ERROR,
        }
    }

    /// Converts to `async_graphql::Error` with `extensions.code` set (async-graphql has a
    /// conflicting blanket `From<T: Display>`, so this is explicit instead of `From`).
    pub fn into_graphql(self) -> async_graphql::Error {
        let code = self.code();
        let mut e = async_graphql::Error::new(self.to_string());
        e.extensions = Some({
            let mut map = async_graphql::ErrorExtensionValues::default();
            map.set("code", code);
            map
        });
        e
    }
}

/// Implement `IntoResponse` so we can return a `Result<T, KabiPayError>` directly from axum handlers.
impl axum::response::IntoResponse for KabiPayError {
    fn into_response(self) -> axum::response::Response {
        let status = self.http_status();
        let body = axum::Json(serde_json::json!({
            "error": {
                "code": self.code(),
                "message": self.to_string(),
            }
        }));
        (status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tenant_database_unavailable_is_retryable_without_internal_details() {
        let tenant_id = Uuid::parse_str("e6d4fc13-feb8-52a0-93bd-f66c795969b1").unwrap();
        let err = KabiPayError::TenantDatabaseUnavailable(tenant_id);
        assert_eq!(err.code(), "TENANT_DATABASE_UNAVAILABLE");
        assert_eq!(
            err.http_status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
        assert_eq!(
            err.to_string(),
            "organization workspace is temporarily unavailable: e6d4fc13-feb8-52a0-93bd-f66c795969b1"
        );

        let timeout = KabiPayError::from_tenant_db(
            tenant_id,
            sea_orm::DbErr::ConnectionAcquire(sea_orm::ConnAcquireErr::Timeout),
        );
        assert_eq!(timeout.code(), "TENANT_DATABASE_UNAVAILABLE");
        assert_eq!(
            timeout.http_status(),
            axum::http::StatusCode::SERVICE_UNAVAILABLE
        );
    }
}
