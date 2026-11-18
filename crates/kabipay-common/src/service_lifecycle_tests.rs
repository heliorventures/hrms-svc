use super::*;
use axum::{
    body::Body,
    http::{Request, StatusCode},
    routing::get,
    Router,
};
use tower::Service;

#[tokio::test]
async fn health_is_live_before_initialization_and_business_is_gated_across_recovery() {
    let state = Readiness::default();
    let app = health_router(state.clone());
    for (path, status) in [
        ("/livez", 200),
        ("/readyz", 503),
        ("/healthz", 503),
        ("/business", 503),
    ] {
        assert_eq!(
            app.clone()
                .call(Request::get(path).body(Body::empty()).unwrap())
                .await
                .unwrap()
                .status()
                .as_u16(),
            status
        );
    }
    state
        .install(Router::new().route("/business", get(|| async { "business" })))
        .unwrap();
    state.set_ready(true);
    assert_eq!(
        app.clone()
            .call(Request::get("/business").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    state.set_ready(false);
    assert_eq!(
        app.clone()
            .call(Request::get("/livez").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::OK
    );
    assert_eq!(
        app.clone()
            .call(Request::get("/business").body(Body::empty()).unwrap())
            .await
            .unwrap()
            .status(),
        StatusCode::SERVICE_UNAVAILABLE
    );
    state.set_ready(true);
    assert!(state.is_ready());
    state.drain();
    state.set_ready(true);
    assert!(!state.is_ready());
    assert!(state.install(Router::new()).is_err());
}
#[test]
fn configuration_and_retry_limits_are_strict_and_bounded() {
    let cfg = LifecycleConfig::from_lookup(|_| None).unwrap();
    assert!(cfg.retry_delay(30, 1000) <= cfg.retry_max);
    for value in ["0", "1", "-1", "2garbage", "300001"] {
        assert!(LifecycleConfig::from_lookup(|key| {
            if key == "KABIPAY_DEPENDENCY_RETRY_INITIAL_MS" {
                Some(value.into())
            } else {
                None
            }
        })
        .is_err());
    }
}

#[tokio::test]
async fn actual_listener_survives_dependency_loss_and_initializes_workers_once() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    let available = Arc::new(AtomicBool::new(false));
    let starts = Arc::new(AtomicUsize::new(0));
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let config = LifecycleConfig {
        retry_initial: Duration::from_millis(1),
        retry_max: Duration::from_millis(2),
        probe_interval: Duration::from_millis(5),
        operation_timeout: Duration::from_millis(100),
    };
    let initialization = config.clone();
    let dependency = available.clone();
    let workers = starts.clone();
    let task = tokio::spawn(serve(listener, config, move |_| async move {
        retry_dependency(&initialization, || async {
            if dependency.load(Ordering::SeqCst) {
                Ok(())
            } else {
                Err(DependencyFailure::temporary())
            }
        })
        .await?;
        workers.fetch_add(1, Ordering::SeqCst);
        let check: HealthCheck = Arc::new(move || {
            let dependency = dependency.clone();
            Box::pin(async move {
                if dependency.load(Ordering::SeqCst) {
                    Ok(())
                } else {
                    Err(DependencyFailure::temporary())
                }
            })
        });
        Ok(PreparedService {
            router: Router::new().route("/business", get(|| async { "ok" })),
            check,
        })
    }));
    let client = reqwest::Client::builder()
        .no_proxy()
        .timeout(Duration::from_secs(1))
        .build()
        .unwrap();
    async fn expect(
        client: &reqwest::Client,
        address: std::net::SocketAddr,
        path: &str,
        status: u16,
    ) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                if let Ok(response) = client.get(format!("http://{address}{path}")).send().await {
                    if response.status().as_u16() == status {
                        return;
                    }
                }
                tokio::time::sleep(Duration::from_millis(5)).await;
            }
        })
        .await
        .unwrap();
    }
    expect(&client, address, "/livez", 200).await;
    expect(&client, address, "/readyz", 503).await;
    for _ in 0..2 {
        available.store(true, Ordering::SeqCst);
        expect(&client, address, "/readyz", 200).await;
        expect(&client, address, "/business", 200).await;
        available.store(false, Ordering::SeqCst);
        expect(&client, address, "/readyz", 503).await;
        expect(&client, address, "/business", 503).await;
        expect(&client, address, "/livez", 200).await;
    }
    assert_eq!(starts.load(Ordering::SeqCst), 1);
    assert!(!task.is_finished());
    task.abort();
    let _ = task.await;
}
#[tokio::test]
async fn dependency_retry_initializes_once_after_transient_failures_and_rejects_fatal_errors() {
    let mut cfg = LifecycleConfig::from_lookup(|_| None).unwrap();
    cfg.retry_initial = std::time::Duration::from_millis(1);
    cfg.retry_max = cfg.retry_initial;
    let calls = std::sync::atomic::AtomicUsize::new(0);
    let value = retry_dependency(&cfg, || async {
        if calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst) < 2 {
            Err(DependencyFailure::temporary())
        } else {
            Ok(42)
        }
    })
    .await
    .unwrap();
    assert_eq!(value, 42);
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 3);
    let calls = std::sync::atomic::AtomicUsize::new(0);
    assert!(retry_dependency(&cfg, || async {
        calls.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        Err::<(), _>(DependencyFailure::fatal())
    })
    .await
    .is_err());
    assert_eq!(calls.load(std::sync::atomic::Ordering::SeqCst), 1);
}
