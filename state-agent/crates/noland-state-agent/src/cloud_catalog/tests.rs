use super::*;
use std::sync::atomic::AtomicUsize;
use std::time::Duration;
use tokio::sync::{Barrier, Notify};

use crate::index_test_support::{root, TestProvider};

async fn wait_for_subscribers(refreshes: &CatalogRefreshes, key: &str, count: usize) {
    tokio::time::timeout(Duration::from_secs(5), async {
        loop {
            if refreshes
                .flights
                .lock()
                .get(key)
                .and_then(Weak::upgrade)
                .is_some_and(|flight| flight.result.receiver_count() == count)
            {
                return;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn overlapping_refreshes_coalesce_but_later_requests_always_refresh() {
    let refreshes = Arc::new(CatalogRefreshes::default());
    let calls = Arc::new(AtomicUsize::new(0));
    let barrier = Arc::new(Barrier::new(9));
    let release = Arc::new(Notify::new());
    let mut tasks = Vec::new();
    for _ in 0..8 {
        let (refreshes, calls, barrier, release) = (
            refreshes.clone(),
            calls.clone(),
            barrier.clone(),
            release.clone(),
        );
        tasks.push(tokio::spawn(async move {
            barrier.wait().await;
            refreshes
                .refresh("repository-key".into(), || async {
                    calls.fetch_add(1, Ordering::Relaxed);
                    release.notified().await;
                    Ok(json!({"catalog_metrics":{}, "catalog_freshness":{}}))
                })
                .await
                .unwrap()
        }));
    }
    barrier.wait().await;
    wait_for_subscribers(&refreshes, "repository-key", 8).await;
    release.notify_one();
    let mut leaders = 0;
    for task in tasks {
        let value = task.await.unwrap();
        leaders += usize::from(value["catalog_freshness"]["coalesced"] == false);
    }
    assert_eq!(calls.load(Ordering::Relaxed), 1);
    assert_eq!(leaders, 1);
    refreshes
        .refresh("repository-key".into(), || async {
            calls.fetch_add(1, Ordering::Relaxed);
            Ok(json!({"catalog_metrics":{}, "catalog_freshness":{}}))
        })
        .await
        .unwrap();
    assert_eq!(
        calls.load(Ordering::Relaxed),
        2,
        "no TTL or completed-result cache"
    );
    assert!(refreshes.flights.lock().is_empty());
}

#[tokio::test]
async fn cancelled_failed_and_invalidated_refreshes_can_be_retried() {
    let refreshes = Arc::new(CatalogRefreshes::default());
    let leader = {
        let refreshes = refreshes.clone();
        tokio::spawn(async move {
            refreshes
                .refresh("repository".into(), || {
                    std::future::pending::<Result<Value>>()
                })
                .await
        })
    };
    wait_for_subscribers(&refreshes, "repository", 1).await;
    let follower = {
        let refreshes = refreshes.clone();
        tokio::spawn(async move {
            refreshes
                .refresh("repository".into(), || async { panic!("must coalesce") })
                .await
        })
    };
    wait_for_subscribers(&refreshes, "repository", 2).await;
    leader.abort();
    assert!(follower.await.unwrap().is_err());
    assert!(refreshes
        .refresh("repository".into(), || async {
            Err(StateError::Storage("remote failure, not empty".into()))
        })
        .await
        .is_err());
    let release = Arc::new(Notify::new());
    let invalidated = {
        let (refreshes, release) = (refreshes.clone(), release.clone());
        tokio::spawn(async move {
            refreshes
                .refresh("repository".into(), || async {
                    release.notified().await;
                    Ok(json!({"catalog_metrics":{}, "catalog_freshness":{}}))
                })
                .await
        })
    };
    wait_for_subscribers(&refreshes, "repository", 1).await;
    refreshes.invalidate();
    let fresh = refreshes
        .refresh("repository".into(), || async {
            Ok(json!({"revision":"new", "catalog_metrics":{}, "catalog_freshness":{}}))
        })
        .await
        .unwrap();
    assert_eq!(fresh["revision"], "new");
    release.notify_one();
    assert!(invalidated.await.unwrap().is_err());
    assert!(refreshes.flights.lock().is_empty());
}

#[tokio::test]
async fn catalog_and_pending_reads_overlap_and_transport_failure_is_never_empty() {
    let root = root("catalog-overlap");
    let mut provider = TestProvider::new(root.join("cloud"));
    provider.overlap = Some(Arc::new(Barrier::new(2)));
    let key = MasterKey::generate();
    let response = tokio::time::timeout(
        Duration::from_secs(5),
        load_from_provider(&provider, &key, 0),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(
        response["catalog_freshness"]["source"],
        "live_remote_validation"
    );
    assert_eq!(
        response["catalog_freshness"]["authoritative_for_execution"],
        false
    );
    assert_eq!(response["pending_deletions"], json!([]));
    assert!(response["catalog_metrics"]["load_catalog_duration_ms"].is_u64());
    provider.overlap = None;
    provider
        .controls
        .fail_pending
        .store(true, Ordering::Relaxed);
    assert!(load_from_provider(&provider, &key, 0).await.is_err());
    provider
        .controls
        .fail_pending
        .store(false, Ordering::Relaxed);
    provider.controls.fail_reads.store(true, Ordering::Relaxed);
    assert!(load_from_provider(&provider, &key, 0).await.is_err());
    provider.controls.fail_reads.store(false, Ordering::Relaxed);
    assert!(load_from_provider(&provider, &key, 0).await.is_ok());
    std::fs::remove_dir_all(root).unwrap();
}

#[tokio::test]
async fn independent_repositories_share_a_bounded_refresh_budget() {
    let refreshes = Arc::new(CatalogRefreshes::default());
    let active = Arc::new(AtomicUsize::new(0));
    let peak = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(Semaphore::new(0));
    let mut tasks = Vec::new();
    for i in 0..8 {
        let (refreshes, active, peak, release) = (
            refreshes.clone(),
            active.clone(),
            peak.clone(),
            release.clone(),
        );
        tasks.push(tokio::spawn(async move {
            refreshes
                .refresh(format!("repository-{i}"), || async {
                    let count = active.fetch_add(1, Ordering::AcqRel) + 1;
                    peak.fetch_max(count, Ordering::Relaxed);
                    release.acquire().await.unwrap().forget();
                    active.fetch_sub(1, Ordering::AcqRel);
                    Ok(json!({"catalog_metrics":{}, "catalog_freshness":{}}))
                })
                .await
                .unwrap()
        }));
    }
    tokio::time::timeout(Duration::from_secs(5), async {
        while active.load(Ordering::Acquire) != 2 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    release.add_permits(8);
    for task in tasks {
        task.await.unwrap();
    }
    assert_eq!(peak.load(Ordering::Relaxed), 2);
    assert!(refreshes.flights.lock().is_empty());
}
