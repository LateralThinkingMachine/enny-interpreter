use super::*;
use std::sync::{Barrier, mpsc as std_mpsc};
#[path = "cause_tests.rs"]
mod cause_tests;

#[test]
fn first_stop_is_one_atomic_reason_and_cancellation_does_not_wait_for_memory() {
    let (pilot, _) = Pilot::new(crate::engine::disabled(), Duration::from_secs(60), 1).unwrap();
    let stopping = pilot.stop_receiver();
    let mut memory = pilot.memory.lock().unwrap();
    pilot.record_issue(
        &mut memory,
        Source::RemoteMix,
        Some(Route::Live),
        "text_limit",
    );
    let original_issue = memory.last_issue;
    let other = pilot.clone();
    let (done, finished) = std_mpsc::channel();
    let worker = std::thread::spawn(move || {
        other.stop("backpressure");
        done.send(()).unwrap();
    });
    let deadline = Instant::now() + Duration::from_secs(1);
    while !*stopping.borrow() && Instant::now() < deadline {
        std::thread::yield_now();
    }
    let cancelled_before_unlock = *stopping.borrow();
    let snapshot = pilot.stopped.snapshot();
    // A delayed source callback holding the same state mutex must not replace
    // the observation retained before the independent Stop latch won.
    pilot.record_issue(&mut memory, Source::SelfSource, None, "provider_closed");
    let retained_issue = memory.last_issue;
    drop(memory);
    finished.recv_timeout(Duration::from_secs(1)).unwrap();
    worker.join().unwrap();
    assert!(cancelled_before_unlock);
    assert_eq!(retained_issue, original_issue);
    assert_eq!(snapshot.unwrap().code, StopCode::Backpressure);
    assert!(pilot.is_stopped());
    pilot.stop("operator_stop");
    assert_eq!(pilot.stopped.snapshot(), snapshot);
    assert_eq!(pilot.generation(), 2);
}

#[test]
fn concurrent_stops_retain_one_complete_winning_code_and_elapsed_value() {
    let latch = Arc::new(StopLatch::new());
    let barrier = Arc::new(Barrier::new(3));
    let workers = [
        (StopCode::Deadline, 50),
        (StopCode::OperatorDisconnected, 37),
    ]
    .map(|(code, elapsed)| {
        let latch = latch.clone();
        let barrier = barrier.clone();
        std::thread::spawn(move || {
            barrier.wait();
            (latch.try_stop(code, elapsed), code, elapsed)
        })
    });
    barrier.wait();
    let results = workers.map(|worker| worker.join().unwrap());
    let winners: Vec<_> = results.iter().filter(|(won, _, _)| *won).collect();
    assert_eq!(winners.len(), 1);
    let (_, code, elapsed_seconds) = *winners[0];
    assert_eq!(
        latch.snapshot(),
        Some(StopDiagnostic {
            code,
            elapsed_seconds
        })
    );
}

#[test]
fn elapsed_values_and_unknown_codes_are_bounded_without_retaining_raw_strings() {
    let latch = StopLatch::new();
    assert!(latch.try_stop(StopCode::from_raw("raw-provider-message"), u64::MAX));
    assert_eq!(
        serde_json::to_value(latch.snapshot()).unwrap(),
        json!({"code":"unknown","elapsed_seconds":MAX_ELAPSED_SECONDS})
    );
    assert_eq!(
        IssueCode::from_raw("raw-provider-message"),
        IssueCode::Unknown
    );
    assert_eq!(IssueCode::from_raw("text_limit"), IssueCode::TextLimit);
    let shutdown = StopLatch::new();
    assert!(shutdown.try_stop(StopCode::from_raw("process_shutdown"), 2));
    assert_eq!(
        serde_json::to_value(shutdown.snapshot()).unwrap(),
        json!({"code":"process_shutdown","elapsed_seconds":2})
    );
}

#[test]
fn asr_usage_numbers_are_bounded_and_never_attached_to_another_route_or_code() {
    let (pilot, _) = Pilot::new(crate::engine::disabled(), Duration::from_secs(60), 1).unwrap();
    let mut memory = pilot.memory.lock().unwrap();
    for (submitted_ms, allowed_ms, reported_ms, expected) in [
        (
            200,
            1_200,
            Some(1_201),
            Some(json!({"submitted_ms":200,"allowed_ms":1200,"reported_ms":1201})),
        ),
        (
            120_000,
            121_000,
            Some(MAX_ELAPSED_SECONDS),
            Some(
                json!({"submitted_ms":120000,"allowed_ms":121000,"reported_ms":MAX_ELAPSED_SECONDS}),
            ),
        ),
        (
            200,
            1_200,
            Some(u64::MAX),
            Some(json!({"submitted_ms":200,"allowed_ms":1200,"reported_ms":null})),
        ),
        (
            0,
            1_000,
            None,
            Some(json!({"submitted_ms":0,"allowed_ms":1000,"reported_ms":null})),
        ),
        (120_001, 121_001, Some(121_002), None),
        (200, 1_201, Some(1_202), None),
        (200, 1_200, Some(1_200), None),
        (200, 1_200, Some(0), None),
        (u32::MAX, 0, Some(1), None),
    ] {
        let error = ProviderError::AsrUsageMismatch {
            submitted_ms,
            reported_ms,
            allowed_ms,
        };
        assert_eq!(error.code(), "usage_mismatch");
        assert!(pilot.record_provider_issue(
            &mut memory,
            Source::RemoteMix,
            Some(Route::Transcription),
            error
        ));
        let json = serde_json::to_value(memory.last_issue.unwrap()).unwrap();
        assert_eq!(json.get("asr_usage"), expected.as_ref());
        assert_eq!(
            json.as_object().unwrap().len(),
            if expected.is_some() { 5 } else { 4 }
        );
    }
    for route in [None, Some(Route::Live), Some(Route::Translation)] {
        pilot.record_provider_issue(
            &mut memory,
            Source::RemoteMix,
            route,
            ProviderError::AsrUsageMismatch {
                submitted_ms: 200,
                reported_ms: Some(1_201),
                allowed_ms: 1_200,
            },
        );
        assert!(memory.last_issue.unwrap().asr_usage.is_none());
    }
    pilot.record_provider_issue(
        &mut memory,
        Source::RemoteMix,
        Some(Route::Transcription),
        ProviderError::UsageMismatch,
    );
    assert!(memory.last_issue.unwrap().asr_usage.is_none());
    pilot.record_issue(
        &mut memory,
        Source::RemoteMix,
        Some(Route::Transcription),
        "usage_mismatch",
    );
    assert!(memory.last_issue.unwrap().asr_usage.is_none());
}
