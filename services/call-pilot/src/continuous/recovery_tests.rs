use super::*;
use crate::engine::{Provenance, ProviderEndReason, TranscriptItem};

async fn recovery(h: &mut StreamHandle, next: u64, seconds: u64) {
    assert!(matches!(h.events.recv().await,
        Some(StreamEvent::Recovering { window, error, retry_in_seconds })
        if window == next && retry_in_seconds == seconds && error.recoverable()));
}

#[tokio::test(start_paused = true)]
async fn interrupted_source_retires_locally_before_recovery_without_final_or_refund() {
    let mut r = rig_with(Duration::from_secs(5), false);
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    w.events
        .send(StreamEvent::Transcript {
            provenance: Provenance::SourceAsr,
            text: "synthetic partial".into(),
            final_fragment: false,
            item: Some(TranscriptItem {
                source_audio: None,
                id: "partial".into(),
                sequence: 1,
                revision: 1,
            }),
        })
        .await
        .unwrap();
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Transcript {
            final_fragment: false,
            ..
        })
    ));
    w.events
        .send(StreamEvent::Error(ProviderError::ConnectionLost))
        .await
        .unwrap();
    recovery(&mut h, 2, 1).await;
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert_eq!(r.closes.load(Ordering::SeqCst), 0);
    assert!(r.backend.enabled());
    assert!(r.backend.failure_cause().is_none());
    for _ in 0..100 {
        h.input
            .send(StreamInput::Pcm16(vec![7; 640]))
            .await
            .unwrap();
        tokio::task::yield_now().await;
    }
    tokio::time::advance(Duration::from_secs(5)).await;
    tokio::task::yield_now().await;
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    let mut next = r.windows.recv().await.unwrap();
    assert_eq!(r.closes.load(Ordering::SeqCst), 1);
    ready(&next, &mut h).await;
    assert!(
        next.input.try_recv().is_err(),
        "discarded audio must not replay"
    );
    assert_eq!(
        r.backend
            .budget_snapshot()
            .continuous
            .unwrap()
            .allocated_microusd,
        170_000
    );
    assert!(
        h.events.try_recv().is_err(),
        "no old final or successful drain is invented"
    );
    h.stop.send_replace(true);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn recoverable_failure_keeps_healthy_sibling_owned_and_processing() {
    let mut r = rig();
    let mut source = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let w = r.windows.recv().await.unwrap();
    ready(&w, &mut source).await;
    let mut live = r
        .backend
        .open_with_mode(Route::Live, Language::En, Some(LiveMode::Interpretation))
        .await
        .unwrap();
    let mut interpreter = r.windows.recv().await.unwrap();
    ready(&interpreter, &mut live).await;
    w.events
        .send(StreamEvent::Error(ProviderError::InputInterrupted))
        .await
        .unwrap();
    recovery(&mut source, 2, 1).await;
    live.input
        .send(StreamInput::Pcm16(vec![0; 640]))
        .await
        .unwrap();
    assert!(matches!(
        interpreter.input.recv().await,
        Some(StreamInput::Pcm16(_))
    ));
    assert!(!*interpreter.stop.borrow());
    assert!(live.events.try_recv().is_err());
    let next = r.windows.recv().await.unwrap();
    ready(&next, &mut source).await;
    assert_eq!(r.opens.load(Ordering::SeqCst), 3);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn startup_recovery_is_ordered_and_retries_are_rate_bounded_for_ninety_minutes() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    let beginning = Instant::now();
    // All fail before Ready: repeated reconnects cannot repeatedly reset backoff.
    for attempt in 1..=184 {
        let start = Instant::now();
        w.events
            .send(StreamEvent::Error(ProviderError::ConnectionLost))
            .await
            .unwrap();
        let expected = match attempt {
            1 => 1,
            2 => 2,
            3 => 4,
            _ => 30,
        };
        recovery(&mut h, attempt + 1, expected).await;
        w = r.windows.recv().await.unwrap();
        assert!(start.elapsed() >= Duration::from_secs(expected));
        assert_eq!(r.closes.load(Ordering::SeqCst), attempt as usize);
        assert_eq!(r.opens.load(Ordering::SeqCst), attempt as usize + 1);
        assert!(r.backend.enabled());
    }
    assert!(beginning.elapsed() >= Duration::from_secs(90 * 60));
    ready(&w, &mut h).await;
    assert_eq!(
        r.backend
            .budget_snapshot()
            .continuous
            .unwrap()
            .windows_started,
        185
    );
    h.stop.send_replace(true);
    r.backend.shutdown().await;
}

#[test]
fn only_stable_service_resets_backoff_and_fatal_errors_never_recover() {
    let mut retry = RetryCadence::default();
    assert_eq!(
        [
            retry.delay(Duration::ZERO),
            retry.delay(Duration::ZERO),
            retry.delay(Duration::ZERO),
            retry.delay(Duration::ZERO)
        ],
        [1, 2, 4, 30]
    );
    assert_eq!(retry.delay(Duration::from_secs(59)), 30);
    assert_eq!(retry.delay(Duration::from_secs(60)), 1);
    for error in [
        ProviderError::Unavailable,
        ProviderError::Disabled,
        ProviderError::Budget,
        ProviderError::UsageMismatch,
        ProviderError::AsrUsageMismatch {
            submitted_ms: 200,
            reported_ms: Some(2000),
            allowed_ms: 1200,
        },
        ProviderError::Protocol,
        ProviderError::TextLimit,
        ProviderError::SourceResponseDeadline,
        ProviderError::SourceIncomplete,
        ProviderError::SessionEnded(ProviderEndReason::Content),
        ProviderError::SessionEnded(ProviderEndReason::CloseRequested),
    ] {
        assert!(!error.recoverable(), "{error:?}");
    }
    for reason in [
        ProviderEndReason::Expired,
        ProviderEndReason::RemoteHangup,
        ProviderEndReason::ConnectionLost,
    ] {
        assert!(ProviderError::SessionEnded(reason).recoverable());
    }
}

#[tokio::test(start_paused = true)]
async fn stop_or_finish_during_recovery_never_admits_another_child() {
    for finish in [false, true] {
        for delay in [Duration::ZERO, Duration::from_secs(5)] {
            let mut r = rig_with(delay, true);
            let mut h = r
                .backend
                .open(Route::Transcription, Language::Ja)
                .await
                .unwrap();
            let w = r.windows.recv().await.unwrap();
            ready(&w, &mut h).await;
            w.events
                .send(StreamEvent::Error(ProviderError::ConnectionLost))
                .await
                .unwrap();
            recovery(&mut h, 2, 1).await;
            if finish {
                h.renewal_fence.as_ref().unwrap().send_replace(true);
                h.input.send(StreamInput::FinishInput).await.unwrap();
                assert!(matches!(
                    h.events.recv().await,
                    Some(StreamEvent::Error(ProviderError::SourceIncomplete))
                ));
            } else {
                h.stop.send_replace(true);
            }
            r.backend.shutdown().await;
            assert_eq!(r.opens.load(Ordering::SeqCst), 1);
            assert_eq!(r.closes.load(Ordering::SeqCst), 1);
            assert!(*w.stop.borrow());
            while let Ok(event) = h.events.try_recv() {
                assert!(!matches!(
                    event,
                    StreamEvent::Ready { .. }
                        | StreamEvent::Renewing { .. }
                        | StreamEvent::Recovering { .. }
                ));
            }
        }
    }
}

#[tokio::test(start_paused = true)]
async fn stalled_retirement_exhausts_local_cleanup_bound_without_overlap() {
    let mut r = rig_with(Duration::from_secs(CLOSE_SECONDS + 1), true);
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    w.events
        .send(StreamEvent::Error(ProviderError::ConnectionLost))
        .await
        .unwrap();
    recovery(&mut h, 2, 1).await;
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Error(ProviderError::Unavailable))
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert!(!r.backend.enabled());
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn ninety_minute_quiet_speech_window_simulation_requires_no_second_start() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    let beginning = Instant::now();
    for number in 1..=44 {
        w.events
            .send(StreamEvent::Ready {
                expires_in_seconds: 300,
            })
            .await
            .unwrap();
        assert!(matches!(
            h.events.recv().await,
            Some(StreamEvent::Ready { .. })
        ));
        // Silence is not an instruction to Finish. This supervisor test uses a
        // virtual quiet interval; actual native silent PCM is tested separately.
        tokio::time::advance(Duration::from_secs(100)).await;
        assert!(h.events.try_recv().is_err());
        w.events
            .send(StreamEvent::Transcript {
                provenance: Provenance::SourceAsr,
                text: "fictional speech after a pause".into(),
                final_fragment: true,
                item: Some(TranscriptItem {
                    source_audio: None,
                    id: "per_window".into(),
                    sequence: 1,
                    revision: 1,
                }),
            })
            .await
            .unwrap();
        assert!(matches!(
            h.events.recv().await,
            Some(StreamEvent::Transcript {
                final_fragment: true,
                ..
            })
        ));
        tokio::time::advance(Duration::from_secs(25)).await;
        assert!(matches!(h.events.recv().await, Some(StreamEvent::Handover)));
        assert!(matches!(
            w.input.recv().await,
            Some(StreamInput::FinishAtBoundary)
        ));
        complete(&w).await;
        assert!(
            matches!(h.events.recv().await, Some(StreamEvent::Renewing { window }) if window == number + 1)
        );
        w = r.windows.recv().await.unwrap();
        assert_eq!(r.closes.load(Ordering::SeqCst), number as usize);
        assert!(r.backend.enabled());
    }
    assert!(beginning.elapsed() >= Duration::from_secs(90 * 60));
    h.stop.send_replace(true);
    r.backend.shutdown().await;
    assert_eq!(
        r.opens.load(Ordering::SeqCst),
        r.closes.load(Ordering::SeqCst)
    );
}
