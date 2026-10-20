//! Source-company authorization, command receipts and protected transport.
mod installation;
mod jobs;
pub(crate) mod membership_lifecycle;
mod routes;
mod transport;
pub use jobs::refresh_payroll_job_authority;
pub use routes::routes;
mod bootstrap;
mod capture;
pub use capture::capture_once;
mod payload;
pub use bootstrap::{bootstrap_once, queue_bootstrap};
pub use payload::*;
pub use transport::*;

use kabipay_common::{KabiPayError, KabiPayResult};
use kabipay_organization_protocol::OrganizationCommand;

pub fn parse_command(body: &[u8]) -> KabiPayResult<OrganizationCommand> {
    if body.len() > 65536 {
        return Err(KabiPayError::Validation(
            "Organization command exceeds 64 KiB".into(),
        ));
    }
    let value: OrganizationCommand = serde_json::from_slice(body)?;
    if value.expected_version <= 0
        || value.command_id.is_nil()
        || value.resource_id.is_nil()
        || !value.payload.is_object()
    {
        return Err(KabiPayError::Validation(
            "Invalid organization command identifiers, version or payload".into(),
        ));
    }
    Ok(value)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rejects_oversize_and_unknown_command_fields() {
        assert!(parse_command(&vec![b' '; 65537]).is_err());
        let id = uuid::Uuid::new_v4();
        let body = format!(
            r#"{{"command_id":"{id}","resource_id":"{id}","expected_version":1,"operation":"directory","payload":{{}},"sql":"select 1"}}"#
        );
        assert!(parse_command(body.as_bytes()).is_err());
    }
    #[test]
    fn rejects_nonpositive_versions_and_nil_ids() {
        let id = uuid::Uuid::new_v4();
        let body = format!(
            r#"{{"command_id":"{id}","resource_id":"{id}","expected_version":0,"operation":"directory","payload":{{}}}}"#
        );
        assert!(parse_command(body.as_bytes()).is_err());
    }
    #[derive(Clone)]
    struct NeverQuery;
    impl SourceAdapter for NeverQuery {
        fn service(&self) -> &'static str {
            "kabipay-leave"
        }
        async fn employee(
            &self,
            _: &sea_orm::DatabaseConnection,
            _: uuid::Uuid,
            _: &OrganizationCommand,
        ) -> KabiPayResult<Option<uuid::Uuid>> {
            panic!("unauthenticated source employee query")
        }
        async fn execute(
            &self,
            _: &sea_orm::DatabaseTransaction,
            _: &sea_orm::DatabaseConnection,
            _: &VerifiedRuntimeAuthorization,
            _: &OrganizationCommand,
        ) -> KabiPayResult<serde_json::Value> {
            panic!("unauthenticated business execution")
        }
    }
    #[tokio::test]
    async fn missing_credentials_fail_before_any_source_lookup() {
        let tenant = uuid::Uuid::new_v4();
        let id = uuid::Uuid::new_v4();
        let body=serde_json::to_vec(&serde_json::json!({"command_id":id,"resource_id":id,"expected_version":1,"operation":"leave_detail","payload":{}})).unwrap();
        let disconnected = sea_orm::DatabaseConnection::Disconnected;
        let error = run(
            &NeverQuery,
            tenant,
            &disconnected,
            &disconnected,
            &axum::http::HeaderMap::new(),
            &body,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, KabiPayError::Unauthorised));
    }
}
