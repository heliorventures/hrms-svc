use axum::{extract::Request, middleware::Next, response::Response};
use std::time::{Duration, Instant};
use uuid::Uuid;

const SLOW_PHASE: Duration = Duration::from_millis(250);

fn phase_is_slow(elapsed: Duration) -> bool {
    elapsed >= SLOW_PHASE
}

pub async fn record_request(req: Request, next: Next) -> Response {
    let method = req.method().clone();
    let path = req.uri().path().to_string();
    let started = Instant::now();
    let response = next.run(req).await;
    let status = response.status();
    let latency_ms = started.elapsed().as_millis() as u64;

    if status.is_server_error() {
        tracing::error!(%method, path, %status, latency_ms, "auth request completed");
    } else if status.is_client_error() {
        tracing::warn!(%method, path, %status, latency_ms, "auth request completed");
    } else {
        tracing::debug!(%method, path, %status, latency_ms, "auth request completed");
    }

    response
}

pub fn record_phase(phase: &'static str, tenant_id: Option<Uuid>, started: Instant) {
    let elapsed = started.elapsed();
    let elapsed_ms = elapsed.as_millis() as u64;
    if phase_is_slow(elapsed) {
        tracing::warn!(phase, ?tenant_id, elapsed_ms, "slow auth phase");
    } else {
        tracing::debug!(phase, ?tenant_id, elapsed_ms, "auth phase");
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn phase_threshold_is_250_milliseconds() {
        assert!(!phase_is_slow(Duration::from_millis(249)));
        assert!(phase_is_slow(Duration::from_millis(250)));
    }
}
