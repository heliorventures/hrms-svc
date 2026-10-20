//! Deployment-owned Auth route plane selection; never selected by a request.
use axum::{routing::get, Router};
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AuthPlane {
    All,
    Ops,
    Client,
}
impl AuthPlane {
    pub fn parse(value: Option<&str>) -> anyhow::Result<Self> {
        match value {
            None | Some("all") => Ok(Self::All),
            Some("ops") => Ok(Self::Ops),
            Some("client") => Ok(Self::Client),
            _ => anyhow::bail!("KABIPAY_AUTH_PLANE must be all, ops or client"),
        }
    }
    pub fn from_env() -> anyhow::Result<Self> {
        match std::env::var("KABIPAY_AUTH_PLANE") {
            Ok(value) => Self::parse(Some(&value)),
            Err(std::env::VarError::NotPresent) => Self::parse(None),
            Err(_) => anyhow::bail!("KABIPAY_AUTH_PLANE must be valid UTF-8"),
        }
    }
    pub fn includes_client(self) -> bool {
        self != Self::Ops
    }
    pub fn routes<S: Clone + Send + Sync + 'static>(
        self,
        ops: Router<S>,
        client: Router<S>,
    ) -> Router<S> {
        let health = Router::new().route("/healthz", get(|| async { "ok" }));
        match self {
            Self::All => health.merge(ops).merge(client),
            Self::Ops => health.merge(ops),
            Self::Client => health.merge(client),
        }
    }
}
#[cfg(test)]
mod tests {
    use super::*;
    use axum::{
        body::Body,
        http::{Request, StatusCode},
        routing::post,
    };
    use tower::Service;
    #[test]
    fn deployment_plane_is_explicit_and_invalid_values_fail_closed() {
        assert_eq!(AuthPlane::parse(None).unwrap(), AuthPlane::All);
        for value in ["all", "ops", "client"] {
            assert!(AuthPlane::parse(Some(value)).is_ok());
        }
        for value in ["", "tenant", "OPS"] {
            assert!(AuthPlane::parse(Some(value)).is_err());
        }
        assert!(!AuthPlane::Ops.includes_client());
    }
    #[tokio::test]
    async fn disabled_routes_are_absent_and_health_remains_available() {
        for plane in [AuthPlane::All, AuthPlane::Ops, AuthPlane::Client] {
            let ops = Router::new().route("/auth/ops/login", post(|| async { "ops" }));
            let client = Router::new()
                .route("/auth/client/login", post(|| async { "client" }))
                .route("/auth/introspect", post(|| async { "introspect" }))
                .route(
                    "/auth/internal/account-eligibility",
                    post(|| async { "private" }),
                );
            let mut router = plane.routes(ops, client);
            for (path, enabled) in [
                ("/auth/ops/login", plane != AuthPlane::Client),
                ("/auth/client/login", plane.includes_client()),
                ("/auth/introspect", plane.includes_client()),
                (
                    "/auth/internal/account-eligibility",
                    plane.includes_client(),
                ),
            ] {
                let response = router
                    .clone()
                    .call(Request::post(path).body(Body::empty()).unwrap())
                    .await
                    .unwrap();
                assert_eq!(
                    response.status(),
                    if enabled {
                        StatusCode::OK
                    } else {
                        StatusCode::NOT_FOUND
                    },
                    "{plane:?} {path}"
                );
            }
            assert_eq!(
                router
                    .call(Request::get("/healthz").body(Body::empty()).unwrap())
                    .await
                    .unwrap()
                    .status(),
                StatusCode::OK
            );
        }
    }
}
