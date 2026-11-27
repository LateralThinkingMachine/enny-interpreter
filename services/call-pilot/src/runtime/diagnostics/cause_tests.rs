//! Secondary cleanup cannot become the reported cause of a paired failure.
use super::*;
use crate::engine::{PreparedAudio, ProviderError, ProviderFailure, ProviderFuture};

struct FailedBackend(Option<ProviderFailure>);
impl ProviderBackend for FailedBackend {
    fn enabled(&self) -> bool {
        false
    }
    fn failure_cause(&self) -> Option<ProviderFailure> {
        self.0
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}

fn fixture(
    cause: Option<ProviderFailure>,
    capacity: usize,
) -> (
    Arc<Pilot>,
    mpsc::Receiver<Value>,
    [watch::Receiver<bool>; 2],
) {
    let (pilot, _) = Pilot::new_persistent(
        Arc::new(FailedBackend(cause)),
        1,
        auth::LocalOrigins::default(),
    )
    .unwrap();
    let (events, received) = mpsc::channel(capacity);
    let (asr_stop, asr_stopped) = watch::channel(false);
    let (live_stop, live_stopped) = watch::channel(false);
    {
        let mut memory = pilot.memory.lock().unwrap();
        memory.operator_connection = Some(Connection {
            id: "synthetic_connection".into(),
            session: "synthetic_session".into(),
            events,
            heartbeat: Instant::now(),
        });
        for (id, source, route, stop) in [
            (
                "original_asr",
                Source::SelfSource,
                Route::Transcription,
                asr_stop,
            ),
            ("cancelled_live", Source::RemoteMix, Route::Live, live_stop),
        ] {
            let (input, _received_input) = mpsc::channel(8);
            memory.routes.insert(
                id.into(),
                ActiveRoute {
                    source,
                    route,
                    language: Language::Ja,
                    live_mode: (route == Route::Live).then_some(LiveMode::Interpretation),
                    interpretation: None,
                    input: Some(input),
                    stop: Some(stop),
                    cancel: watch::channel(false).0,
                    renewal_fence: None,
                    ready: true,
                    renewing: false,
                    handover: false,
                    finish_sent: false,
                },
            );
        }
    }
    (pilot, received, [asr_stopped, live_stopped])
}
fn source_failure() -> ProviderFailure {
    ProviderFailure {
        route: Route::Transcription,
        error: ProviderError::SpeechBoundaryLimit,
    }
}

#[test]
fn sibling_closure_keeps_the_original_route_source_and_duplicate_cleanup_is_idempotent() {
    let (pilot, mut events, stopped) = fixture(Some(source_failure()), 8);
    // The remote interpreter's cancellation is delivered before the source's
    // own delayed error. Original ASR is bound to the self source, not remote.
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    let gap = events.try_recv().unwrap();
    assert_eq!(gap["code"], "speech_boundary_limit");
    let original = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(original.code, IssueCode::SpeechBoundaryLimit);
    assert_eq!(original.route, Some(Route::Transcription));
    assert_eq!(original.source, Source::SelfSource);
    assert!(*stopped[1].borrow());
    assert!(!*stopped[0].borrow());
    // Pause may fence the other source before that original event is consumed.
    pilot.source_gap(Source::SelfSource, "source_gap");
    pilot.source_gap(Source::RemoteMix, "source_gap");
    pilot.route_gap("original_asr", Source::SelfSource, "provider_closed");
    assert_eq!(pilot.memory.lock().unwrap().last_issue, Some(original));
    assert!(pilot.memory.lock().unwrap().routes.is_empty());
    assert!(*stopped[0].borrow());
    assert!(!pilot.enabled());
    assert!(!pilot.is_stopped());
    assert!(
        pilot
            .start_route(1, Source::SelfSource, Route::Transcription, Language::Ja)
            .is_err()
    );
}

#[test]
fn source_gap_before_the_provider_callback_preserves_the_already_latched_cause() {
    let (pilot, mut events, stopped) = fixture(Some(source_failure()), 8);
    // The browser can observe a local capture failure and fence the source
    // before its queued provider Error is consumed. The backend's first cause
    // is already known, and its actual route still identifies the right source.
    pilot.source_gap(Source::SelfSource, "source_gap");
    assert_eq!(events.try_recv().unwrap()["code"], "speech_boundary_limit");
    let original = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(original.code, IssueCode::SpeechBoundaryLimit);
    assert_eq!(original.route, Some(Route::Transcription));
    assert_eq!(original.source, Source::SelfSource);
    assert!(*stopped[0].borrow());
    pilot.route_gap("original_asr", Source::SelfSource, "provider_closed");
    pilot.source_gap(Source::RemoteMix, "source_gap");
    assert_eq!(pilot.memory.lock().unwrap().last_issue, Some(original));
    assert!(*stopped[1].borrow());
    assert!(pilot.memory.lock().unwrap().routes.is_empty());
    assert!(!pilot.enabled());
}

#[test]
fn equal_prior_issue_fields_do_not_fabricate_resolved_cause_provenance() {
    let failure = ProviderFailure {
        route: Route::Transcription,
        error: ProviderError::Unavailable,
    };
    let (pilot, _events, _) = fixture(Some(failure), 8);
    {
        let mut memory = pilot.memory.lock().unwrap();
        // A pending duplicate's refusal can have the same kind and code as
        // the later retained fault, while describing a different source.
        pilot.record_issue(
            &mut memory,
            Source::RemoteMix,
            Some(Route::Transcription),
            "provider_unavailable",
        );
        assert!(memory.resolved_provider_failure.is_none());
    }
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    let memory = pilot.memory.lock().unwrap();
    let original = memory.last_issue.unwrap();
    assert_eq!(original.code, IssueCode::ProviderUnavailable);
    assert_eq!(original.source, Source::SelfSource);
    assert_eq!(memory.resolved_provider_failure, Some((failure, original)));
}

#[test]
fn repeated_source_gap_preserves_a_finite_original_issue_without_causal_metadata() {
    let (pilot, _events, _) = fixture(None, 8);
    pilot.route_gap(
        "original_asr",
        Source::SelfSource,
        "source_response_deadline",
    );
    let original = pilot.memory.lock().unwrap().last_issue;
    pilot.source_gap(Source::SelfSource, "source_gap");
    pilot.source_gap(Source::SelfSource, "provider_closed");
    assert_eq!(pilot.memory.lock().unwrap().last_issue, original);
}

#[test]
fn a_pending_duplicate_does_not_borrow_the_admitted_original_source() {
    let (pilot, mut events, _) = fixture(Some(source_failure()), 8);
    pilot.memory.lock().unwrap().routes.insert(
        "another_pending_asr".into(),
        ActiveRoute {
            source: Source::RemoteMix,
            route: Route::Transcription,
            language: Language::Ja,
            live_mode: None,
            interpretation: None,
            input: None,
            stop: None,
            cancel: watch::channel(false).0,
            renewal_fence: None,
            ready: false,
            renewing: false,
            handover: false,
            finish_sent: false,
        },
    );
    // Both source bindings exist, but only the original has an admitted handle.
    // The callback may be on Live's sibling channel without borrowing its source.
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    assert_eq!(events.try_recv().unwrap()["code"], "speech_boundary_limit");
    let issue = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(issue.code, IssueCode::SpeechBoundaryLimit);
    assert_eq!(issue.route, Some(Route::Transcription));
    assert_eq!(issue.source, Source::SelfSource);
    pilot.route_gap("original_asr", Source::SelfSource, "provider_closed");
    assert_eq!(pilot.memory.lock().unwrap().last_issue, Some(issue));
}

#[test]
fn an_unadmitted_binding_cannot_acquire_causal_source_attribution_after_cleanup() {
    let (pilot, mut events, _) = fixture(Some(source_failure()), 8);
    {
        let mut memory = pilot.memory.lock().unwrap();
        let pending = memory.routes.get_mut("original_asr").unwrap();
        pending.input = None;
        pending.stop = None;
    }
    // A backend failure can be known before its accepted handle is installed
    // in runtime. Until then this binding has no evidence it owns that failure.
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    assert_eq!(events.try_recv().unwrap()["code"], "provider_closed");
    let observed = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(observed.route, Some(Route::Live));
    assert_eq!(observed.source, Source::RemoteMix);
    pilot.source_gap(Source::SelfSource, "source_gap");
    let local = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(local.code, IssueCode::SourceGap);
    assert_eq!(local.route, None);
    assert_eq!(local.source, Source::SelfSource);
}

#[test]
fn multiple_admitted_matches_never_guess_the_original_source_by_map_order() {
    let (pilot, mut events, _) = fixture(Some(source_failure()), 8);
    {
        let mut memory = pilot.memory.lock().unwrap();
        // Defensive case for a future backend: a kind-only cause cannot name
        // either source when two accepted handles match it.
        let other = memory.routes.get_mut("cancelled_live").unwrap();
        other.route = Route::Transcription;
        other.live_mode = None;
    }
    pilot.source_gap(Source::RemoteMix, "source_gap");
    assert_eq!(events.try_recv().unwrap()["code"], "source_gap");
    let local = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(local.code, IssueCode::SourceGap);
    assert_eq!(local.route, None);
    assert_eq!(local.source, Source::RemoteMix);
}

#[test]
fn missing_original_route_never_attributes_its_kind_to_the_sibling_source() {
    let (pilot, mut events, _) = fixture(Some(source_failure()), 8);
    pilot.memory.lock().unwrap().routes.remove("original_asr");
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    assert_eq!(events.try_recv().unwrap()["code"], "provider_closed");
    let issue = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(issue.code, IssueCode::ProviderClosed);
    assert_eq!(issue.route, Some(Route::Live));
    assert_eq!(issue.source, Source::RemoteMix);
}

#[test]
fn saturated_event_delivery_and_late_stop_callbacks_preserve_fixed_first_cause() {
    let (pilot, _events, _) = fixture(Some(source_failure()), 1);
    assert!(Pilot::queue_private(
        &pilot.memory.lock().unwrap(),
        json!({"type":"synthetic_full_queue"})
    ));
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    assert!(pilot.is_stopped());
    assert_eq!(
        pilot.stopped.snapshot().unwrap().code,
        StopCode::Backpressure
    );
    let original = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(original.code, IssueCode::SpeechBoundaryLimit);
    assert_eq!(original.source, Source::SelfSource);
    pilot.source_gap(Source::SelfSource, "source_gap");
    pilot.stop("operator_stop");
    assert_eq!(pilot.memory.lock().unwrap().last_issue, Some(original));
}

#[test]
fn numeric_asr_first_cause_survives_secondary_cleanup_without_borrowing_its_source() {
    let failure = ProviderFailure {
        route: Route::Transcription,
        error: ProviderError::AsrUsageMismatch {
            submitted_ms: 1_100,
            reported_ms: Some(2_101),
            allowed_ms: 2_100,
        },
    };
    let (pilot, mut events, _) = fixture(Some(failure), 8);
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    let gap = events.try_recv().unwrap();
    assert_eq!(gap["code"], "usage_mismatch");
    assert!(gap.get("asr_usage").is_none());
    let original = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(original.source, Source::SelfSource);
    assert_eq!(original.route, Some(Route::Transcription));
    assert_eq!(
        original.asr_usage,
        Some(AsrUsageDiagnostic {
            submitted_ms: 1_100,
            reported_ms: Some(2_101),
            allowed_ms: 2_100,
        })
    );
    pilot.source_gap(Source::SelfSource, "source_gap");
    pilot.source_gap(Source::RemoteMix, "source_gap");
    pilot.stop("operator_stop");
    pilot.route_failure(
        "original_asr",
        Source::SelfSource,
        ProviderError::AsrUsageMismatch {
            submitted_ms: 1,
            reported_ms: Some(9_999),
            allowed_ms: 1_001,
        },
    );
    let memory = pilot.memory.lock().unwrap();
    assert_eq!(memory.last_issue, Some(original));
    assert_eq!(memory.resolved_provider_failure, Some((failure, original)));
}

#[test]
fn unresolved_numeric_cause_does_not_leak_into_a_sibling_observation() {
    let (pilot, _events, _) = fixture(
        Some(ProviderFailure {
            route: Route::Transcription,
            error: ProviderError::AsrUsageMismatch {
                submitted_ms: 200,
                reported_ms: Some(1_300),
                allowed_ms: 1_200,
            },
        }),
        8,
    );
    pilot.memory.lock().unwrap().routes.remove("original_asr");
    pilot.route_gap("cancelled_live", Source::RemoteMix, "provider_closed");
    let issue = pilot.memory.lock().unwrap().last_issue.unwrap();
    assert_eq!(issue.code, IssueCode::ProviderClosed);
    assert_eq!(issue.source, Source::RemoteMix);
    assert_eq!(issue.route, Some(Route::Live));
    assert!(issue.asr_usage.is_none());
}
