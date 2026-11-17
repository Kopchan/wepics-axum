//! `ThumbHub` deduplicates concurrent generation and releases keys on failure.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use wepics_axum::hubs::thumb::ThumbHub;

#[tokio::test]
async fn concurrent_requests_share_one_generation() {
    let hub = ThumbHub::default();
    let runs = Arc::new(AtomicUsize::new(0));

    // Both calls are created before either is polled, so the second one must
    // attach to the in-flight task instead of starting another.
    let (first, second) = tokio::join!(
        hub.get_or_generate("thumb/key".into(), {
            let runs = Arc::clone(&runs);
            move || async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(vec![1, 2, 3])
            }
        }),
        hub.get_or_generate("thumb/key".into(), {
            let runs = Arc::clone(&runs);
            move || async move {
                runs.fetch_add(1, Ordering::SeqCst);
                Ok(vec![4, 5, 6])
            }
        }),
    );

    let first = first.expect("first call succeeds");
    let second = second.expect("second call succeeds");
    assert_eq!(runs.load(Ordering::SeqCst), 1, "only one task renders");
    assert!(
        Arc::ptr_eq(&first, &second),
        "callers share the same buffer without copying"
    );
    assert_eq!(*first, vec![1, 2, 3]);
}

#[tokio::test]
async fn key_is_released_after_an_error() {
    let hub = ThumbHub::default();

    let failed = hub
        .get_or_generate("thumb/key".into(), || async {
            Err("render failed".to_string())
        })
        .await;
    assert_eq!(failed.unwrap_err(), "render failed");

    // The guard removed the key, so a retry starts a fresh task.
    let retried = hub
        .get_or_generate("thumb/key".into(), || async { Ok(vec![7]) })
        .await;
    assert_eq!(*retried.expect("retry succeeds"), vec![7]);
}

#[tokio::test]
async fn different_keys_generate_independently() {
    let hub = ThumbHub::default();

    let (small, large) = tokio::join!(
        hub.get_or_generate("thumb/100".into(), || async { Ok(vec![1]) }),
        hub.get_or_generate("thumb/800".into(), || async { Ok(vec![2]) }),
    );

    assert_eq!(*small.expect("small succeeds"), vec![1]);
    assert_eq!(*large.expect("large succeeds"), vec![2]);
}
