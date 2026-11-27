use super::*;
use std::sync::atomic::{AtomicUsize, Ordering};
#[path = "failure_tests.rs"]
mod failure_tests;
#[path = "recovery_tests.rs"]
mod recovery_tests;
#[path = "speech_hints_tests.rs"]
mod speech_hints_tests;

fn policy() -> ContinuousPolicy {
    ContinuousPolicy {
        route_duration_seconds: 300,
        max_concurrent_sessions: 2,
        max_input_audio_seconds: 600,
        max_output_audio_seconds: 600,
        max_text_characters: 76800,
        warning_threshold_microusd: 10000,
        allowed_routes: vec![Route::Transcription, Route::Live],
        allowed_live_modes: vec![LiveMode::Interpretation],
    }
}

#[tokio::test]
async fn process_allocations_survive_backend_retirement_without_resetting_estimates() {
    let totals = Arc::new(ProcessAllocations::default());
    let factory: Factory =
        Arc::new(|_, _, _| panic!("no provider is opened by allocation or rollover"));
    let first =
        ContinuousBackend::with_factory_allocations(policy(), factory.clone(), totals.clone());
    first.shared.allocate(Route::Transcription).unwrap();
    first.shared.allocate(Route::Live).unwrap();
    let before = first.budget_snapshot().continuous.unwrap();
    assert_eq!(before.windows_started, 2);
    assert!(first.retire().await);
    let next = ContinuousBackend::with_factory_allocations(policy(), factory, totals);
    let fresh = next.budget_snapshot().continuous.unwrap();
    assert_eq!(fresh.windows_started, before.windows_started);
    assert_eq!(fresh.allocated_microusd, before.allocated_microusd);
    next.shared.allocate(Route::Transcription).unwrap();
    let after = next.budget_snapshot().continuous.unwrap();
    assert_eq!(after.windows_started, 3);
    assert_eq!(
        after.allocated_microusd,
        before.allocated_microusd + stream_cost(Route::Transcription, 300).unwrap()
    );
    assert!(next.retire().await);
}
struct Mock {
    handle: Mutex<Option<StreamHandle>>,
    closes: Arc<AtomicUsize>,
    delay: Duration,
    certain: bool,
}
impl ProviderBackend for Mock {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        let handle = self.handle.lock().unwrap().take();
        Box::pin(async { handle.ok_or(ProviderError::Unavailable) })
    }
    fn open_with_mode(
        &self,
        route: Route,
        language: Language,
        _: Option<LiveMode>,
    ) -> ProviderFuture<StreamHandle> {
        self.open(route, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}
struct Owner(Arc<Mock>);
impl WindowOwner for Owner {
    fn backend(&self) -> Arc<dyn ProviderBackend> {
        self.0.clone()
    }
    fn shutdown(&self) -> Shutdown<'_> {
        Box::pin(async {
            tokio::time::sleep(self.0.delay).await;
            self.0.closes.fetch_add(1, Ordering::SeqCst);
            Retirement {
                released: true,
                confirmed: self.0.certain,
                remote_closed: self.0.certain,
            }
        })
    }
}
struct Witness {
    events: mpsc::Sender<StreamEvent>,
    input: mpsc::Receiver<StreamInput>,
    stop: watch::Receiver<bool>,
}
struct Rig {
    backend: ContinuousBackend,
    windows: mpsc::UnboundedReceiver<Witness>,
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
}
fn rig() -> Rig {
    rig_with(Duration::ZERO, true)
}
fn rig_with(delay: Duration, certain: bool) -> Rig {
    rig_with_routes(delay, [certain, certain])
}
fn rig_with_routes(delay: Duration, certainty: [bool; 2]) -> Rig {
    rig_with_delays([delay, delay], certainty)
}
fn rig_with_delays(delays: [Duration; 2], certainty: [bool; 2]) -> Rig {
    let (tx, windows) = mpsc::unbounded_channel();
    let opens = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let created = opens.clone();
    let retired = closes.clone();
    let factory: Factory = Arc::new(move |route, _, _| {
        created.fetch_add(1, Ordering::SeqCst);
        let (input, input_rx) = mpsc::channel(8);
        let (events, event_rx) = mpsc::channel(32);
        let (stop, stop_rx) = watch::channel(false);
        tx.send(Witness {
            events,
            input: input_rx,
            stop: stop_rx,
        })
        .unwrap();
        Ok(Arc::new(Owner(Arc::new(Mock {
            handle: Mutex::new(Some(StreamHandle {
                input,
                events: event_rx,
                stop,
                renewal_fence: None,
            })),
            closes: retired.clone(),
            delay: delays[slot(route).unwrap()],
            certain: certainty[slot(route).unwrap()],
        }))))
    });
    Rig {
        backend: ContinuousBackend::with_factory(policy(), factory),
        windows,
        opens,
        closes,
    }
}
async fn ready(w: &Witness, h: &mut StreamHandle) {
    w.events
        .send(StreamEvent::Ready {
            expires_in_seconds: 180,
        })
        .await
        .unwrap();
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Ready { .. })
    ));
}
async fn handover(w: &mut Witness, h: &mut StreamHandle) {
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(matches!(h.events.recv().await, Some(StreamEvent::Handover)));
    assert!(matches!(
        w.input.recv().await,
        Some(StreamInput::FinishAtBoundary)
    ));
}
async fn complete(w: &Witness) {
    w.events.send(StreamEvent::InputSealed).await.unwrap();
    w.events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    w.events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
}

#[test]
fn policy_is_exact_and_does_not_expand_scope() {
    assert!(policy().validate().is_ok());
    let mut invalid = policy();
    invalid.allowed_live_modes.push(LiveMode::Assistant);
    assert_eq!(invalid.validate(), Err(ProviderError::Protocol));
    let mut invalid = policy();
    invalid.max_concurrent_sessions = 3;
    assert_eq!(invalid.validate(), Err(ProviderError::Protocol));
    let mut invalid = policy();
    invalid.route_duration_seconds = 3600;
    assert_eq!(invalid.validate(), Err(ProviderError::Protocol));
    assert!(ContinuousBackend::new("short".into(), policy()).is_err());
}

#[tokio::test(start_paused = true)]
async fn cancelled_outer_shutdown_aborts_owned_supervisors_and_latches_uncertainty() {
    let mut r = rig_with(Duration::from_secs(5), true);
    let _handle = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let witness = r.windows.recv().await.unwrap();
    let mut shutdown = Box::pin(r.backend.shutdown());
    assert!(matches!(
        futures_util::poll!(shutdown.as_mut()),
        std::task::Poll::Pending
    ));
    drop(shutdown);
    tokio::task::yield_now().await;
    assert!(*witness.stop.borrow());
    assert_eq!(r.backend.budget_snapshot().state, BudgetState::Uncertain);
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert!(
        r.backend
            .open(Route::Transcription, Language::Ja)
            .await
            .is_err()
    );
}

#[tokio::test(start_paused = true)]
async fn many_confirmed_windows_continue_past_warning_without_overlap_or_refund() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    for window in 1..=40 {
        ready(&w, &mut h).await;
        let spending = r.backend.budget_snapshot();
        assert!(spending.amounts.is_none());
        assert!(spending.remaining_seconds.is_none());
        let c = spending.continuous.unwrap();
        assert_eq!(c.windows_started, window);
        assert_eq!(c.allocated_microusd, window * 85000);
        assert!(c.allocated_microusd > c.warning_threshold_microusd);
        assert!(r.backend.enabled());
        assert_eq!(r.opens.load(Ordering::SeqCst), window as usize);
        assert_eq!(r.closes.load(Ordering::SeqCst), window as usize - 1);
        handover(&mut w, &mut h).await;
        complete(&w).await;
        assert!(
            matches!(h.events.recv().await, Some(StreamEvent::Renewing { window: next }) if next == window + 1)
        );
        w = r.windows.recv().await.unwrap();
        assert_eq!(r.closes.load(Ordering::SeqCst), window as usize);
    }
    h.stop.send_replace(true);
    r.backend.shutdown().await;
    assert!(!r.backend.enabled());
    assert_eq!(
        r.opens.load(Ordering::SeqCst),
        r.closes.load(Ordering::SeqCst)
    );
}

#[tokio::test(start_paused = true)]
async fn renewal_requires_both_final_source_receipts_and_confirmed_close() {
    for (completion, confirmed, expected) in [
        (
            DrainCompletion::ObservationWindowEnded,
            true,
            ProviderError::SourceIncomplete,
        ),
        (
            DrainCompletion::CommittedTranscriptsFinal,
            false,
            ProviderError::Unavailable,
        ),
    ] {
        let mut r = rig();
        let mut h = r
            .backend
            .open(Route::Transcription, Language::Ja)
            .await
            .unwrap();
        let mut w = r.windows.recv().await.unwrap();
        ready(&w, &mut h).await;
        handover(&mut w, &mut h).await;
        w.events.send(StreamEvent::InputSealed).await.unwrap();
        w.events
            .send(StreamEvent::Drained { completion })
            .await
            .unwrap();
        let _ = w.events.send(StreamEvent::Closed { confirmed }).await;
        assert!(
            matches!(h.events.recv().await, Some(StreamEvent::Error(error)) if error == expected)
        );
        assert_eq!(r.opens.load(Ordering::SeqCst), 1);
        assert!(!r.backend.enabled());
        assert_eq!(r.backend.budget_snapshot().state, BudgetState::Uncertain);
        assert!(
            r.backend
                .open(Route::Transcription, Language::Ja)
                .await
                .is_err()
        );
        r.backend.shutdown().await;
    }
}

#[tokio::test(start_paused = true)]
async fn transition_pcm_is_bounded_and_not_replayed_into_next_window() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    h.input
        .send(StreamInput::Pcm16(vec![1; 640]))
        .await
        .unwrap();
    assert!(matches!(w.input.recv().await, Some(StreamInput::Pcm16(bytes)) if bytes[0] == 1));
    handover(&mut w, &mut h).await;
    // Until the natural boundary, old-window PCM still reaches the detector.
    h.input
        .send(StreamInput::Pcm16(vec![2; 640]))
        .await
        .unwrap();
    assert!(matches!(w.input.recv().await, Some(StreamInput::Pcm16(bytes)) if bytes[0] == 2));
    w.events.send(StreamEvent::InputSealed).await.unwrap();
    tokio::task::yield_now().await;
    for _ in 0..100 {
        h.input
            .send(StreamInput::Pcm16(vec![3; 640]))
            .await
            .unwrap();
        tokio::task::yield_now().await;
    }
    assert!(w.input.try_recv().is_err());
    w.events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    w.events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Renewing { window: 2 })
    ));
    let mut second = r.windows.recv().await.unwrap();
    ready(&second, &mut h).await;
    assert!(second.input.try_recv().is_err());
    h.input
        .send(StreamInput::Pcm16(vec![4; 640]))
        .await
        .unwrap();
    assert!(matches!(second.input.recv().await, Some(StreamInput::Pcm16(bytes)) if bytes[0] == 4));
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn stop_wins_over_ready_or_confirmed_renewal_and_never_reopens() {
    for during_drain in [false, true] {
        let mut r = rig();
        let mut h = r
            .backend
            .open(Route::Transcription, Language::Ja)
            .await
            .unwrap();
        let mut w = r.windows.recv().await.unwrap();
        if during_drain {
            ready(&w, &mut h).await;
            handover(&mut w, &mut h).await;
            complete(&w).await;
        }
        h.stop.send_replace(true);
        r.backend.shutdown().await;
        assert!(*w.stop.borrow());
        assert_eq!(r.opens.load(Ordering::SeqCst), 1);
        while let Ok(event) = h.events.try_recv() {
            assert!(!matches!(
                event,
                StreamEvent::Renewing { .. } | StreamEvent::Ready { .. }
            ));
        }
    }
}

#[tokio::test(start_paused = true)]
async fn manual_finish_is_forwarded_once_and_does_not_renew() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    h.input.send(StreamInput::FinishInput).await.unwrap();
    assert!(matches!(
        w.input.recv().await,
        Some(StreamInput::FinishInput)
    ));
    w.events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    w.events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Drained { .. })
    ));
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Closed { confirmed: true })
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn exact_scope_concurrency_and_checked_counters_remain_enforced() {
    let mut r = rig();
    let _asr = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    assert!(matches!(
        r.backend.open(Route::Transcription, Language::Ja).await,
        Err(ProviderError::StreamLimit)
    ));
    assert!(
        r.backend
            .open(Route::Translation, Language::Ja)
            .await
            .is_err()
    );
    assert!(r.backend.open(Route::Live, Language::Ja).await.is_err());
    let _live = r
        .backend
        .open_with_mode(Route::Live, Language::Ja, Some(LiveMode::Interpretation))
        .await
        .unwrap();
    let _ = r.windows.recv().await;
    let _ = r.windows.recv().await;
    assert_eq!(
        r.backend
            .budget_snapshot()
            .stream_reservations
            .unwrap()
            .used,
        2
    );
    r.backend.shared.ledger.lock().unwrap().allocated = MAX_SAFE;
    assert_eq!(
        r.backend.shared.allocate(Route::Live),
        Err(ProviderError::ProtocolCapacityLimit)
    );
    assert_eq!(r.backend.shared.ledger.lock().unwrap().allocated, MAX_SAFE);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn finish_fence_beats_queued_seal_drain_and_close_without_new_admission() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    handover(&mut w, &mut h).await;
    complete(&w).await;
    h.renewal_fence.as_ref().unwrap().send_replace(true);
    h.input.send(StreamInput::FinishInput).await.unwrap();
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Closed { confirmed: true })
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn slow_cleanup_consumes_transition_pcm_and_finish_adopts_completed_window() {
    let mut r = rig_with(Duration::from_secs(5), true);
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    handover(&mut w, &mut h).await;
    complete(&w).await;
    tokio::task::yield_now().await;
    for _ in 0..100 {
        h.input
            .send(StreamInput::Pcm16(vec![0; 640]))
            .await
            .unwrap();
        tokio::task::yield_now().await;
    }
    assert!(h.events.try_recv().is_err());
    h.renewal_fence.as_ref().unwrap().send_replace(true);
    h.input.send(StreamInput::FinishInput).await.unwrap();
    tokio::time::advance(Duration::from_secs(5)).await;
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Drained { .. })
    ));
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Closed { confirmed: true })
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert_eq!(r.closes.load(Ordering::SeqCst), 1);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn returned_but_uncertain_cleanup_never_authorizes_a_new_window() {
    let mut r = rig_with(Duration::ZERO, false);
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    handover(&mut w, &mut h).await;
    complete(&w).await;
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Error(ProviderError::Unavailable))
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    assert!(!r.backend.enabled());
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn a_valid_long_utterance_can_cross_handover_without_forced_finish() {
    let mut r = rig();
    let mut h = r
        .backend
        .open(Route::Transcription, Language::Ja)
        .await
        .unwrap();
    let mut w = r.windows.recv().await.unwrap();
    ready(&w, &mut h).await;
    handover(&mut w, &mut h).await;
    for _ in 0..100 {
        h.input
            .send(StreamInput::Pcm16(vec![0; 640]))
            .await
            .unwrap();
        assert!(matches!(w.input.recv().await, Some(StreamInput::Pcm16(_))));
        tokio::time::advance(Duration::from_secs(1)).await;
    }
    assert!(h.events.try_recv().is_err());
    assert_eq!(r.opens.load(Ordering::SeqCst), 1);
    complete(&w).await;
    assert!(matches!(
        h.events.recv().await,
        Some(StreamEvent::Renewing { window: 2 })
    ));
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn finish_cancels_live_but_preserves_the_ordered_source_drain() {
    let mut r = rig_with_routes(Duration::ZERO, [true, false]);
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
    let _interpreter = r.windows.recv().await.unwrap();
    asr.renewal_fence.as_ref().unwrap().send_replace(true);
    live.renewal_fence.as_ref().unwrap().send_replace(true);
    live.stop.send_replace(true);
    tokio::task::yield_now().await;
    assert!(!*r.backend.shared.stop.borrow());
    asr.input.send(StreamInput::FinishInput).await.unwrap();
    source
        .events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    source
        .events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert!(matches!(
        asr.events.recv().await,
        Some(StreamEvent::Drained { .. })
    ));
    assert!(matches!(
        asr.events.recv().await,
        Some(StreamEvent::Closed { confirmed: true })
    ));
    for _ in 0..3 {
        if matches!(live.events.recv().await, Some(StreamEvent::Closed { .. })) {
            break;
        }
    }
    assert_eq!(r.closes.load(Ordering::SeqCst), 2);
    assert!(!*r.backend.shared.stop.borrow());
    r.backend.shutdown().await;
}

#[path = "interpretation_policy_tests.rs"]
mod interpretation_policy_tests;
