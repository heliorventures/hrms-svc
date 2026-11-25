use kabipay_notification::email::worker::{supervise_tenants, WorkerLimits};
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
use uuid::Uuid;
#[tokio::test(start_paused = true)]
async fn transient_fault_backoff_releases_the_healthy_tenant_slot() {
    let failed = Arc::new(AtomicUsize::new(0));
    let healthy = Arc::new(AtomicUsize::new(0));
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let bad = failed.clone();
    let good = healthy.clone();
    let job = tokio::spawn(supervise_tenants(
        vec![Uuid::from_u128(1), Uuid::from_u128(2)],
        WorkerLimits {
            concurrency: 1,
            tick_interval: Duration::from_millis(1),
            operation_timeout: Duration::from_millis(20),
        },
        shutdown,
        move |tenant| {
            let bad = bad.clone();
            let good = good.clone();
            async move {
                if tenant == Uuid::from_u128(1) {
                    bad.fetch_add(1, Ordering::Relaxed);
                    panic!("synthetic tenant fault");
                }
                good.fetch_add(1, Ordering::Relaxed);
                true
            }
        },
    ));
    for _ in 0..64 {
        tokio::time::advance(Duration::from_millis(2)).await;
        tokio::task::yield_now().await;
    }
    stop.send(true).unwrap();
    job.await.unwrap();
    assert!(healthy.load(Ordering::Relaxed) > 6);
    assert!(
        failed.load(Ordering::Relaxed) <= 8,
        "repeated tenant faults must use capped exponential backoff"
    );
}
#[tokio::test]
async fn one_bad_tenant_does_not_starve_others_and_shutdown_is_bounded() {
    let counts = Arc::new([
        AtomicUsize::new(0),
        AtomicUsize::new(0),
        AtomicUsize::new(0),
    ]);
    let (stop, shutdown) = tokio::sync::watch::channel(false);
    let seen = counts.clone();
    let job = tokio::spawn(supervise_tenants(
        vec![Uuid::from_u128(1), Uuid::from_u128(2), Uuid::from_u128(3)],
        WorkerLimits {
            concurrency: 2,
            tick_interval: Duration::from_millis(1),
            operation_timeout: Duration::from_millis(20),
        },
        shutdown,
        move |tenant| {
            let seen = seen.clone();
            async move {
                let index = tenant.as_u128() as usize - 1;
                if index == 0 {
                    std::future::pending::<()>().await;
                }
                seen[index].fetch_add(1, Ordering::Relaxed);
                true
            }
        },
    ));
    tokio::time::timeout(Duration::from_secs(2), async {
        while counts[1].load(Ordering::Relaxed) < 2 || counts[2].load(Ordering::Relaxed) < 2 {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
    })
    .await
    .unwrap();
    stop.send(true).unwrap();
    tokio::time::timeout(Duration::from_secs(1), job)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(counts[0].load(Ordering::Relaxed), 0);
}
