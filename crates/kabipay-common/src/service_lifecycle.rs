//! Process health is independent of dependency readiness and business dispatch.
use axum::{
    body::Body,
    extract::State,
    http::{Request, StatusCode},
    response::{IntoResponse, Response},
    routing::get,
    Json, Router,
};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, OnceLock,
    },
    time::Duration,
};
use tower::Service;

#[derive(Clone, Copy, Debug, thiserror::Error)]
#[error("{code}")]
pub struct DependencyFailure {
    retryable: bool,
    code: &'static str,
}
impl DependencyFailure {
    pub fn temporary() -> Self {
        Self {
            retryable: true,
            code: "DEPENDENCY_UNAVAILABLE",
        }
    }
    pub fn fatal() -> Self {
        Self {
            retryable: false,
            code: "DEPENDENCY_CONFIGURATION_INVALID",
        }
    }
    pub fn retryable(self) -> bool {
        self.retryable
    }
}

#[derive(Clone, Debug)]
pub struct LifecycleConfig {
    pub retry_initial: Duration,
    pub retry_max: Duration,
    pub probe_interval: Duration,
    pub operation_timeout: Duration,
}
impl LifecycleConfig {
    pub fn from_env() -> anyhow::Result<Self> {
        Self::from_lookup(|key| std::env::var_os(key).map(|v| v.to_str().unwrap_or("").to_owned()))
    }
    pub fn from_lookup(mut lookup: impl FnMut(&str) -> Option<String>) -> anyhow::Result<Self> {
        let mut value = |name, fallback, minimum| -> anyhow::Result<Duration> {
            let milliseconds = if let Some(raw) = lookup(name) {
                anyhow::ensure!(
                    !raw.is_empty() && raw.bytes().all(|c| c.is_ascii_digit()),
                    "invalid lifecycle setting: {name}"
                );
                raw.parse::<u64>()?
            } else {
                fallback
            };
            anyhow::ensure!(
                (minimum..=300_000).contains(&milliseconds),
                "invalid lifecycle setting: {name}"
            );
            Ok(Duration::from_millis(milliseconds))
        };
        let result = Self {
            retry_initial: value("KABIPAY_DEPENDENCY_RETRY_INITIAL_MS", 500, 100)?,
            retry_max: value("KABIPAY_DEPENDENCY_RETRY_MAX_MS", 30_000, 100)?,
            probe_interval: value("KABIPAY_DEPENDENCY_PROBE_INTERVAL_MS", 5_000, 250)?,
            operation_timeout: value("KABIPAY_DEPENDENCY_TIMEOUT_MS", 3_000, 100)?,
        };
        anyhow::ensure!(
            result.retry_initial <= result.retry_max,
            "invalid lifecycle retry limits"
        );
        Ok(result)
    }
    pub fn retry_delay(&self, attempt: u32, jitter: u32) -> Duration {
        let base = self
            .retry_initial
            .saturating_mul(1u32 << attempt.min(20))
            .min(self.retry_max);
        let milliseconds = base
            .as_millis()
            .saturating_mul(800 + u128::from(jitter.min(1000)) * 400 / 1000)
            / 1000;
        Duration::from_millis(milliseconds.max(1).min(self.retry_max.as_millis()) as u64)
    }
}

#[derive(Clone, Default)]
pub struct Readiness {
    ready: Arc<AtomicBool>,
    draining: Arc<AtomicBool>,
    app: Arc<OnceLock<Router>>,
}
impl Readiness {
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
            && !self.draining.load(Ordering::Acquire)
            && self.app.get().is_some()
    }
    pub fn set_ready(&self, value: bool) {
        self.ready.store(value, Ordering::Release);
    }
    pub fn drain(&self) {
        self.draining.store(true, Ordering::Release);
        self.set_ready(false);
    }
    pub fn install(&self, router: Router) -> anyhow::Result<()> {
        self.app
            .set(router)
            .map_err(|_| anyhow::anyhow!("service router already initialized"))
    }
}
async fn ready(State(state): State<Readiness>) -> impl IntoResponse {
    (
        if state.is_ready() {
            StatusCode::OK
        } else {
            StatusCode::SERVICE_UNAVAILABLE
        },
        [("cache-control", "no-store")],
        if state.is_ready() {
            "ok"
        } else {
            "unavailable"
        },
    )
}
async fn dispatch(State(state): State<Readiness>, request: Request<Body>) -> Response {
    if state.is_ready() {
        if let Some(app) = state.app.get() {
            return app
                .clone()
                .call(request)
                .await
                .unwrap_or_else(|never| match never {});
        }
    }
    (StatusCode::SERVICE_UNAVAILABLE,[("retry-after","5"),("cache-control","no-store")],Json(serde_json::json!({"error":{"code":"SERVICE_UNAVAILABLE","message":"Service is temporarily unavailable"}}))).into_response()
}
pub fn health_router(state: Readiness) -> Router {
    Router::new()
        .route("/livez", get(|| async { "ok" }))
        .route("/readyz", get(ready))
        .route("/healthz", get(ready))
        .fallback(dispatch)
        .with_state(state)
}
pub type HealthFuture = Pin<Box<dyn Future<Output = Result<(), DependencyFailure>> + Send>>;
pub type HealthCheck = Arc<dyn Fn() -> HealthFuture + Send + Sync>;
pub struct PreparedService {
    pub router: Router,
    pub check: HealthCheck,
}

/// Retry only classified dependency unavailability; never replay business work.
pub async fn retry_dependency<T, F, Fut>(
    config: &LifecycleConfig,
    mut initialize: F,
) -> Result<T, DependencyFailure>
where
    F: FnMut() -> Fut,
    Fut: Future<Output = Result<T, DependencyFailure>>,
{
    let mut attempt = 0;
    loop {
        let result = tokio::time::timeout(config.operation_timeout, initialize())
            .await
            .unwrap_or_else(|_| Err(DependencyFailure::temporary()));
        match result {
            Ok(value) => return Ok(value),
            Err(error) if !error.retryable() => return Err(error),
            Err(_) => {
                tracing::warn!(
                    code = "DEPENDENCY_UNAVAILABLE",
                    "service initialization deferred"
                );
                let jitter = (uuid::Uuid::new_v4().as_u128() % 1001) as u32;
                tokio::time::sleep(config.retry_delay(attempt, jitter)).await;
                attempt = attempt.saturating_add(1);
            }
        }
    }
}

async fn shutdown(state: Readiness) {
    #[cfg(unix)]
    {
        let mut term = tokio::signal::unix::signal(tokio::signal::unix::SignalKind::terminate())
            .expect("SIGTERM handler");
        tokio::select! {_=tokio::signal::ctrl_c()=>{},_=term.recv()=>{}}
    }
    #[cfg(not(unix))]
    {
        let _ = tokio::signal::ctrl_c().await;
    }
    state.drain();
}
pub async fn serve<F, Fut>(
    listener: tokio::net::TcpListener,
    config: LifecycleConfig,
    initialize: F,
) -> anyhow::Result<()>
where
    F: FnOnce(Readiness) -> Fut,
    Fut: Future<Output = anyhow::Result<PreparedService>>,
{
    let state = Readiness::default();
    let health = health_router(state.clone());
    let shutdown_state = state.clone();
    let server = async {
        axum::serve(listener, health)
            .with_graceful_shutdown(shutdown(shutdown_state))
            .await
    };
    let supervise = async {
        let prepared = initialize(state.clone()).await?;
        state.install(prepared.router)?;
        loop {
            let checked = tokio::time::timeout(config.operation_timeout, (prepared.check)())
                .await
                .unwrap_or_else(|_| Err(DependencyFailure::temporary()));
            match checked {
                Ok(()) => state.set_ready(true),
                Err(error) => {
                    state.set_ready(false);
                    if !error.retryable() {
                        return Err(anyhow::Error::new(error));
                    }
                }
            }
            tokio::time::sleep(config.probe_interval).await;
        }
    };
    tokio::select! {result=server=>{state.drain();result.map_err(Into::into)},result=supervise=>{state.drain();result}}
}

#[cfg(test)]
#[path = "service_lifecycle_tests.rs"]
mod tests;
