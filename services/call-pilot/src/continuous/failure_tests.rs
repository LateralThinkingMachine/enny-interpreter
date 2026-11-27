//! Cause delivery must not depend on which paired cleanup completes first.
use super::*;

#[tokio::test(start_paused = true)]
async fn original_fault_is_observable_before_slow_cleanup_and_sibling_closure() {
    let mut r = rig_with_delays([Duration::from_secs(5), Duration::ZERO], [true, true]);
    let mut asr = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let source = r.windows.recv().await.unwrap();
    ready(&source, &mut asr).await;
    let mut live = r
        .backend
        .open_with_mode(Route::Live, Language::En, Some(LiveMode::Interpretation))
        .await
        .unwrap();
    let interpreter = r.windows.recv().await.unwrap();
    ready(&interpreter, &mut live).await;
    let failed_at = Instant::now();
    source
        .events
        .send(StreamEvent::Error(ProviderError::SpeechBoundaryLimit))
        .await
        .unwrap();
    assert!(matches!(
        asr.events.recv().await,
        Some(StreamEvent::Error(ProviderError::SpeechBoundaryLimit))
    ));
    assert!(
        failed_at.elapsed() < Duration::from_secs(5),
        "cause precedes its awaited cleanup"
    );
    assert!(
        r.closes.load(Ordering::SeqCst) <= 1,
        "the original slow cleanup is still pending"
    );
    assert_eq!(
        r.backend.failure_cause(),
        Some(ProviderFailure {
            route: Route::Transcription,
            error: ProviderError::SpeechBoundaryLimit
        })
    );
    assert!(!r.backend.enabled());
    assert!(matches!(
        live.events.recv().await,
        Some(StreamEvent::Closed { confirmed: false })
    ));
    assert_eq!(
        r.closes.load(Ordering::SeqCst),
        1,
        "only the fast sibling is retired"
    );
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(matches!(
        asr.events.recv().await,
        Some(StreamEvent::Closed { confirmed: false })
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 2);
    r.backend.shutdown().await;
    assert_eq!(
        r.backend.failure_cause().unwrap().error,
        ProviderError::SpeechBoundaryLimit
    );
}

#[tokio::test(start_paused = true)]
async fn a_full_event_queue_cannot_erase_or_replace_the_first_failure() {
    let mut r = rig();
    let mut live = r
        .backend
        .open_with_mode(Route::Live, Language::En, Some(LiveMode::Interpretation))
        .await
        .unwrap();
    let interpreter = r.windows.recv().await.unwrap();
    ready(&interpreter, &mut live).await;
    for _ in 0..32 {
        interpreter
            .events
            .send(StreamEvent::Audio {
                pcm16: vec![0, 0],
                sample_rate: 24000,
            })
            .await
            .unwrap();
        tokio::task::yield_now().await;
    }
    interpreter
        .events
        .send(StreamEvent::Error(ProviderError::TextLimit))
        .await
        .unwrap();
    for _ in 0..100 {
        if r.backend.failure_cause().is_some() {
            break;
        }
        tokio::task::yield_now().await;
    }
    let original = ProviderFailure {
        route: Route::Live,
        error: ProviderError::TextLimit,
    };
    assert_eq!(r.backend.failure_cause(), Some(original));
    assert!(!r.backend.enabled());
    assert_eq!(
        r.backend
            .shared
            .remember_failure(Route::Transcription, ProviderError::Unavailable),
        original
    );
    assert_eq!(r.backend.failure_cause(), Some(original));
    live.stop.send_replace(true);
    r.backend.shutdown().await;
    assert_eq!(r.backend.failure_cause(), Some(original));
}

#[tokio::test(start_paused = true)]
async fn route_local_errors_never_borrow_the_retained_sibling_code() {
    let mut r = rig();
    let mut live = r
        .backend
        .open_with_mode(Route::Live, Language::En, Some(LiveMode::Interpretation))
        .await
        .unwrap();
    let interpreter = r.windows.recv().await.unwrap();
    ready(&interpreter, &mut live).await;
    // Model the interval after another supervisor latches its cause but before
    // its poison signal is observed. The causal route may not be installed in
    // runtime yet, so the secondary channel must preserve its own error kind.
    let original = r
        .backend
        .shared
        .remember_failure(Route::Transcription, ProviderError::SpeechBoundaryLimit);
    assert!(!r.backend.enabled());
    assert_eq!(
        r.backend.shared.allocate(Route::Transcription),
        Err(ProviderError::Unavailable),
        "retaining the cause must revoke admission before sibling cancellation"
    );
    interpreter
        .events
        .send(StreamEvent::Error(ProviderError::TextLimit))
        .await
        .unwrap();
    assert!(matches!(
        live.events.recv().await,
        Some(StreamEvent::Error(ProviderError::TextLimit))
    ));
    assert_eq!(r.backend.failure_cause(), Some(original));
    assert!(!r.backend.enabled());
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn explicit_stop_wins_over_a_late_queued_error_without_inventing_a_cause() {
    let mut r = rig();
    let mut asr = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let source = r.windows.recv().await.unwrap();
    ready(&source, &mut asr).await;
    asr.stop.send_replace(true);
    source
        .events
        .send(StreamEvent::Error(ProviderError::TextLimit))
        .await
        .unwrap();
    r.backend.shutdown().await;
    assert!(r.backend.failure_cause().is_none());
    assert!(!r.backend.enabled());
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
}
