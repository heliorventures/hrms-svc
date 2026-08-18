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

    /// A caller-correctable domain rule with a stable machine-readable code.
    ///
    /// Keep `message` free of secrets and database details because REST and
    /// GraphQL clients receive it verbatim.
    #[error("{message}")]
    BusinessRule {
        code: &'static str,
        message: String,
    },

    /// A caller-correctable state or uniqueness conflict with a stable
    /// machine-readable code.
    #[error("{message}")]
    ConflictRule {
        code: &'static str,
        message: String,
    },

    #[error("conflict: {0}")]
    Conflict(String),

    #[error("database operation failed")]
    Database(#[from] sea_orm::DbErr),

    #[error("organization workspace is temporarily unavailable")]
    TenantDatabaseUnavailable(Uuid),

    #[error("JWT error: {0}")]
    Jwt(#[from] jsonwebtoken::errors::Error),

    #[error("JSON error: {0}")]
    Json(#[from] serde_json::Error),

    #[error("internal server error")]
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
            Self::BusinessRule { code, .. } => *code,
            Self::ConflictRule { code, .. } => *code,
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
            Self::Validation(_) | Self::BusinessRule { .. } | Self::Json(_) => S::BAD_REQUEST,
            Self::ConflictRule { .. } | Self::Conflict(_) => S::CONFLICT,
            Self::TenantDatabaseUnavailable(_) => S::SERVICE_UNAVAILABLE,
            Self::Database(_) | Self::Internal(_) => S::INTERNAL_SERVER_ERROR,
        }
    }

    /// Converts to `async_graphql::Error` with `extensions.code` set (async-graphql has a
    /// conflicting blanket `From<T: Display>`, so this is explicit instead of `From`).
    pub fn into_graphql(self) -> async_graphql::Error {
        let code = self.code();
        let error_id = self.record_internal_failure(code);
        let mut e = async_graphql::Error::new(self.public_message());
        e.extensions = Some({
            let mut map = async_graphql::ErrorExtensionValues::default();
            map.set("code", code);
            if let Some(error_id) = error_id {
                map.set("errorId", error_id.to_string());
            }
            map
        });
        e
    }

    /// Emit only allowlisted diagnostic fields. The wrapped database/internal message can
    /// contain SQL, paths, object keys, or PII and must never enter application logs.
    fn record_internal_failure(&self, code: &'static str) -> Option<Uuid> {
        let error_class = match self {
            Self::Database(_) => "DATABASE",
            Self::Internal(_) => "INTERNAL",
            _ => return None,
        };
        let error_id = Uuid::new_v4();
        tracing::error!(
            error_id = %error_id,
            code,
            error_class,
            "request failed with an internal service error"
        );
        Some(error_id)
    }

    fn public_message(&self) -> String {
        match self {
            Self::Database(_) => "database operation failed".into(),
            Self::Internal(_) => "internal server error".into(),
            Self::TenantDatabaseUnavailable(_) => {
                "organization workspace is temporarily unavailable".into()
            }
            _ => self.to_string(),
        }
    }
}

/// Implement `IntoResponse` so we can return a `Result<T, KabiPayError>` directly from axum handlers.
impl axum::response::IntoResponse for KabiPayError {
    fn into_response(self) -> axum::response::Response {
        let status = self.http_status();
        let code = self.code();
        let error_id = self.record_internal_failure(code);
        let mut payload = serde_json::json!({
            "error": {
                "code": code,
                "message": self.public_message(),
            }
        });
        if let Some(error_id) = error_id {
            payload["error"]["errorId"] = serde_json::Value::String(error_id.to_string());
        }
        let body = axum::Json(payload);
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
            "organization workspace is temporarily unavailable"
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

    #[test]
    fn business_rule_preserves_its_public_code() {
        let err = KabiPayError::BusinessRule {
            code: "CURRENT_PASSWORD_INCORRECT",
            message: "The current password is incorrect.".into(),
        };

        assert_eq!(err.code(), "CURRENT_PASSWORD_INCORRECT");
        assert_eq!(err.http_status(), axum::http::StatusCode::BAD_REQUEST);
        assert_eq!(err.to_string(), "The current password is incorrect.");
    }

    #[test]
    fn conflict_rule_preserves_its_public_code_and_conflict_status() {
        let err = KabiPayError::ConflictRule {
            code: "ASSET_TAG_CONFLICT",
            message: "asset tag is already in use".into(),
        };

        assert_eq!(err.code(), "ASSET_TAG_CONFLICT");
        assert_eq!(err.http_status(), axum::http::StatusCode::CONFLICT);
        assert_eq!(err.to_string(), "asset tag is already in use");
    }

    #[test]
    fn database_and_internal_errors_are_sanitized_for_graphql_clients() {
        let database_error = KabiPayError::Database(sea_orm::DbErr::Custom(
            "duplicate key value violates secret_constraint".into(),
        ));
        assert_eq!(database_error.to_string(), "database operation failed");
        let database = database_error.into_graphql();
        assert_eq!(database.message, "database operation failed");
        assert!(!database.message.contains("secret_constraint"));
        let database_extensions = database.extensions.as_ref().unwrap();
        assert!(database_extensions.get("errorId").is_some());

        let internal_error = KabiPayError::Internal("private implementation detail".into());
        assert_eq!(internal_error.to_string(), "internal server error");
        let internal = internal_error.into_graphql();
        assert_eq!(internal.message, "internal server error");
        assert!(!internal.message.contains("private implementation detail"));
        let internal_extensions = internal.extensions.as_ref().unwrap();
        assert!(internal_extensions.get("errorId").is_some());
    }
}
