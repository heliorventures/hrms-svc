//! Structured logging bootstrap.
//!
//! Call `init_tracing()` once at the top of every service's `main()`.

use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

/// Initialise `tracing` with an `EnvFilter` from `RUST_LOG` (default `info,kabipay=debug`)
/// and JSON output for production log ingestion. Dev builds get a pretty compact format.
pub fn init_tracing(service_name: &str) {
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info,kabipay=debug"));

    let json_logs = std::env::var("KABIPAY_LOG_FORMAT").ok().as_deref() == Some("json");

    let registry = tracing_subscriber::registry().with(filter);

    if json_logs {
        registry
            .with(
                fmt::layer()
                    .json()
                    .with_current_span(true)
                    .with_target(true),
            )
            .init();
    } else {
        registry
            .with(fmt::layer().compact().with_target(true))
            .init();
    }

    tracing::info!(service = service_name, "telemetry initialised");
}
