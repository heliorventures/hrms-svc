use super::*;
#[test]
fn network_and_capacity_errors_retry_but_security_and_schema_errors_do_not() {
    for code in ["08001", "08006", "53300", "57P01", "57P02", "57P03"] {
        assert!(retryable_sqlstate(code), "{code}");
    }
    for code in ["28P01", "28000", "42501", "3D000", "42P01", "08P01"] {
        assert!(!retryable_sqlstate(code), "{code}");
    }
    assert!(
        DependencyFailure::from_sqlx(&sqlx::Error::Io(std::io::Error::from(
            std::io::ErrorKind::ConnectionRefused
        )))
        .retryable()
    );
    assert!(DependencyFailure::from_sqlx(&sqlx::Error::PoolTimedOut).retryable());
    for kind in [std::io::ErrorKind::BrokenPipe,std::io::ErrorKind::HostUnreachable,std::io::ErrorKind::NetworkUnreachable] {
        assert!(DependencyFailure::from_sqlx(&sqlx::Error::Io(std::io::Error::from(kind))).retryable(),"{kind:?}");
    }
    assert!(
        !DependencyFailure::from_sqlx(&sqlx::Error::Tls(Box::new(std::io::Error::other(
            "synthetic private certificate failure"
        ))))
        .retryable()
    );
    assert!(
        !DependencyFailure::from_kabipay(&crate::KabiPayError::Forbidden("synthetic".into()))
            .retryable()
    );
    assert!(
        !DependencyFailure::from_kabipay(&crate::KabiPayError::TenantDatabaseUnavailable(
            uuid::Uuid::nil()
        ))
        .retryable()
    );
    assert!(!DependencyFailure::from_seaorm(&sea_orm::DbErr::Custom(
        "connection refused but not a typed transport error".into()
    ))
    .retryable());
}
