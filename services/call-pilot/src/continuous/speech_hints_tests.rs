//! Immutable ASR hint propagation across the real continuous-window supervisor.
//! Provider windows are synthetic controlled peers; this is not language evidence.
use super::*;
use crate::engine::{RouteOptions, SpeechHints};

type Observations = Arc<Mutex<Vec<(Language, SpeechHints)>>>;

struct HintBackend {
    inner: Arc<Mock>,
    observations: Observations,
}
impl ProviderBackend for HintBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
    fn open_transcription(
        &self,
        language: Language,
        hints: SpeechHints,
    ) -> ProviderFuture<StreamHandle> {
        self.observations.lock().unwrap().push((language, hints));
        self.inner.open(Route::Transcription, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}
struct HintOwner {
    backend: Arc<HintBackend>,
    closes: Arc<AtomicUsize>,
}
impl WindowOwner for HintOwner {
    fn backend(&self) -> Arc<dyn ProviderBackend> {
        self.backend.clone()
    }
    fn shutdown(&self) -> Shutdown<'_> {
        Box::pin(async {
            self.closes.fetch_add(1, Ordering::SeqCst);
            Retirement {
                released: true,
                confirmed: true,
                remote_closed: true,
            }
        })
    }
}
fn hints_rig() -> (Rig, Observations) {
    let (tx, windows) = mpsc::unbounded_channel();
    let opens = Arc::new(AtomicUsize::new(0));
    let closes = Arc::new(AtomicUsize::new(0));
    let observations = Arc::new(Mutex::new(Vec::new()));
    let created = opens.clone();
    let retired = closes.clone();
    let observed = observations.clone();
    let factory: Factory = Arc::new(move |route, _, _| {
        assert_eq!(route, Route::Transcription);
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
        Ok(Arc::new(HintOwner {
            backend: Arc::new(HintBackend {
                inner: Arc::new(Mock {
                    handle: Mutex::new(Some(StreamHandle {
                        input,
                        events: event_rx,
                        stop,
                        renewal_fence: None,
                    })),
                    closes: retired.clone(),
                    delay: Duration::ZERO,
                    certain: true,
                }),
                observations: observed.clone(),
            }),
            closes: retired.clone(),
        }))
    });
    (
        Rig {
            backend: ContinuousBackend::with_factory(policy(), factory),
            windows,
            opens,
            closes,
        },
        observations,
    )
}
fn hints() -> SpeechHints {
    SpeechHints {
        languages: vec![Language::Ja, Language::En],
        keywords: vec!["青葉商事".into(), "Acme ZX-4".into()],
    }
}

#[tokio::test(start_paused = true)]
async fn source_language_and_literal_terms_survive_planned_renewal_and_recovery() {
    for (anchor, initial) in [
        (Language::Ja, hints()),
        (
            Language::Vi,
            SpeechHints {
                languages: vec![Language::Vi, Language::Es],
                keywords: vec!["Example-42".into()],
            },
        ),
        (
            Language::Es,
            SpeechHints {
                languages: vec![Language::Es, Language::Vi],
                keywords: vec![],
            },
        ),
    ] {
        let (mut r, observed) = hints_rig();
        let mut h = r
            .backend
            .open_transcription(anchor, initial.clone())
            .await
            .unwrap();
        let mut first = r.windows.recv().await.unwrap();
        ready(&first, &mut h).await;
        handover(&mut first, &mut h).await;
        complete(&first).await;
        assert!(matches!(
            h.events.recv().await,
            Some(StreamEvent::Renewing { window: 2 })
        ));
        let second = r.windows.recv().await.unwrap();
        ready(&second, &mut h).await;
        second
            .events
            .send(StreamEvent::Error(ProviderError::ConnectionLost))
            .await
            .unwrap();
        assert!(matches!(
            h.events.recv().await,
            Some(StreamEvent::Recovering {
                window: 3,
                retry_in_seconds: 1,
                ..
            })
        ));
        let third = r.windows.recv().await.unwrap();
        ready(&third, &mut h).await;
        let snapshots = observed.lock().unwrap().clone();
        assert_eq!(snapshots.len(), 3);
        for (language, received) in snapshots {
            assert_eq!(language, anchor);
            assert!(
                received == initial,
                "every window must receive the identical explicit hints"
            );
        }
        assert_eq!(r.closes.load(Ordering::SeqCst), 2);
        assert_eq!(
            r.backend
                .budget_snapshot()
                .continuous
                .unwrap()
                .windows_started,
            3
        );
        h.stop.send_replace(true);
        r.backend.shutdown().await;
        assert_eq!(
            r.opens.load(Ordering::SeqCst),
            r.closes.load(Ordering::SeqCst)
        );
    }
}

#[tokio::test(start_paused = true)]
async fn expanded_source_anchor_refuses_implicit_or_mismatched_hints_before_window_creation() {
    let (r, observed) = hints_rig();
    for language in [Language::Es, Language::Vi] {
        assert!(
            r.backend
                .open(Route::Transcription, language)
                .await
                .is_err()
        );
        assert!(
            r.backend
                .open_transcription(language, SpeechHints::default())
                .await
                .is_err()
        );
    }
    assert_eq!(r.opens.load(Ordering::SeqCst), 0);
    assert!(observed.lock().unwrap().is_empty());
    assert_eq!(
        r.backend
            .budget_snapshot()
            .continuous
            .unwrap()
            .allocated_microusd,
        0
    );
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn invalid_or_wrong_route_hints_never_create_a_window_or_allocate() {
    let (r, observed) = hints_rig();
    for invalid in [
        SpeechHints {
            languages: Vec::new(),
            keywords: Vec::new(),
        },
        SpeechHints {
            languages: vec![Language::Ja, Language::Ja],
            keywords: Vec::new(),
        },
        SpeechHints {
            languages: vec![Language::En],
            keywords: vec!["<instruction>".into()],
        },
        SpeechHints {
            languages: vec![Language::En],
            keywords: vec!["word".repeat(81)],
        },
    ] {
        assert!(matches!(
            r.backend.open_transcription(Language::En, invalid).await,
            Err(ProviderError::Protocol)
        ));
    }
    assert!(matches!(
        r.backend
            .open_options(
                Route::Live,
                Language::En,
                RouteOptions {
                    live_mode: Some(LiveMode::Interpretation),
                    transcription: Some(hints()),
                    interpretation: None,
                }
            )
            .await,
        Err(ProviderError::Protocol)
    ));
    assert_eq!(r.opens.load(Ordering::SeqCst), 0);
    assert!(observed.lock().unwrap().is_empty());
    let budget = r.backend.budget_snapshot();
    assert_eq!(budget.continuous.unwrap().windows_started, 0);
    assert_eq!(budget.stream_reservations.unwrap().used, 0);
    r.backend.shutdown().await;
}

#[tokio::test(start_paused = true)]
async fn explicit_hints_do_not_bypass_stop_or_finish_renewal_fences() {
    for stop in [false, true] {
        let (mut r, observed) = hints_rig();
        let mut h = r
            .backend
            .open_transcription(Language::Hi, hints())
            .await
            .unwrap();
        let mut first = r.windows.recv().await.unwrap();
        ready(&first, &mut h).await;
        handover(&mut first, &mut h).await;
        complete(&first).await;
        if stop {
            h.stop.send_replace(true);
            r.backend.shutdown().await;
        } else {
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
            r.backend.shutdown().await;
        }
        assert_eq!(r.opens.load(Ordering::SeqCst), 1);
        assert_eq!(observed.lock().unwrap().len(), 1);
        assert_eq!(
            r.backend
                .budget_snapshot()
                .continuous
                .unwrap()
                .windows_started,
            1
        );
    }
}
