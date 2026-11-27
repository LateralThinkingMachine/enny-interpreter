//! Composition checks: real persistent Pilot, continuous orchestration, and
//! bounded synthetic child streams. No sockets, provider requests, or playback.
use super::*;
use crate::{
    auth::{self, ApiError, Role},
    engine::{Provenance, TranscriptItem},
    runtime::{Pilot, Source, Subscription},
};
use axum::http::{HeaderMap, HeaderValue};
use serde_json::Value;
use std::sync::atomic::{AtomicUsize, Ordering};

struct Child(Mutex<Option<StreamHandle>>);
impl ProviderBackend for Child {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        let handle = self.0.lock().unwrap().take();
        Box::pin(async { handle.ok_or(ProviderError::Unavailable) })
    }
    fn open_with_mode(
        &self,
        route: Route,
        language: Language,
        mode: Option<LiveMode>,
    ) -> ProviderFuture<StreamHandle> {
        assert_eq!(
            mode,
            (route == Route::Live).then_some(LiveMode::Interpretation)
        );
        self.open(route, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}
struct Owner {
    child: Arc<Child>,
    closes: Arc<AtomicUsize>,
}
impl WindowOwner for Owner {
    fn backend(&self) -> Arc<dyn ProviderBackend> {
        self.child.clone()
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
struct Window {
    route: Route,
    events: mpsc::Sender<StreamEvent>,
    input: mpsc::Receiver<StreamInput>,
    stop: watch::Receiver<bool>,
    fence: watch::Receiver<bool>,
}
impl Window {
    async fn ready(&self) {
        self.events
            .send(StreamEvent::Ready {
                // Both handover clocks fire after five virtual seconds. Real
                // native boundary detection is exercised in transport tests.
                expires_in_seconds: if self.route == Route::Transcription {
                    180
                } else {
                    50
                },
            })
            .await
            .unwrap();
    }
    async fn next_input(&mut self) -> StreamInput {
        tokio::time::timeout(Duration::from_secs(1), self.input.recv())
            .await
            .unwrap()
            .expect("owned child input")
    }
    async fn source(&self, revision: u32, final_fragment: bool, text: &str) {
        self.events
            .send(StreamEvent::Transcript {
                provenance: Provenance::SourceAsr,
                text: text.to_owned(),
                final_fragment,
                // Reused by the next child to prove the runtime's window scope.
                item: Some(TranscriptItem {
                    source_audio: None,
                    id: "reused_item".into(),
                    sequence: 1,
                    revision,
                }),
            })
            .await
            .unwrap();
    }
    fn complete_planned(&self) {
        if self.route == Route::Transcription {
            self.events.try_send(StreamEvent::InputSealed).unwrap();
        }
        self.events
            .try_send(StreamEvent::Drained {
                completion: if self.route == Route::Transcription {
                    DrainCompletion::CommittedTranscriptsFinal
                } else {
                    DrainCompletion::ObservationWindowEnded
                },
            })
            .unwrap();
        self.events
            .try_send(StreamEvent::Closed { confirmed: true })
            .unwrap();
    }
}
struct Rig {
    pilot: Arc<Pilot>,
    backend: Arc<ContinuousBackend>,
    subscription: Subscription,
    windows: mpsc::Receiver<Window>,
    opens: Arc<AtomicUsize>,
    closes: Arc<AtomicUsize>,
    source_id: String,
    live_id: String,
}
impl Rig {
    fn new() -> Self {
        let (tx, windows) = mpsc::channel(8);
        let opens = Arc::new(AtomicUsize::new(0));
        let closes = Arc::new(AtomicUsize::new(0));
        let created = opens.clone();
        let retired = closes.clone();
        let factory: Factory = Arc::new(move |route, parents, fence| {
            assert!(parents.iter().all(|parent| !*parent.borrow()));
            assert!(!*fence.borrow());
            let (input, input_rx) = mpsc::channel(8);
            let (events, event_rx) = mpsc::channel(32);
            let (stop, stop_rx) = watch::channel(false);
            tx.try_send(Window {
                route,
                events,
                input: input_rx,
                stop: stop_rx,
                fence,
            })
            .unwrap_or_else(|_| panic!("bounded window inventory"));
            created.fetch_add(1, Ordering::SeqCst);
            Ok(Arc::new(Owner {
                child: Arc::new(Child(Mutex::new(Some(StreamHandle {
                    input,
                    events: event_rx,
                    stop,
                    renewal_fence: None,
                })))),
                closes: retired.clone(),
            }))
        });
        let backend = Arc::new(ContinuousBackend::with_factory(
            ContinuousPolicy {
                route_duration_seconds: 300,
                max_concurrent_sessions: 2,
                max_input_audio_seconds: 600,
                max_output_audio_seconds: 600,
                max_text_characters: 76800,
                warning_threshold_microusd: 10000,
                allowed_routes: vec![Route::Transcription, Route::Live],
                allowed_live_modes: vec![LiveMode::Interpretation],
            },
            factory,
        ));
        let (pilot, ticket) =
            Pilot::new_persistent(backend.clone(), 600, auth::LocalOrigins::default()).unwrap();
        let (view, token) = pilot.redeem(&ticket, Role::Operator).unwrap();
        let mut headers = HeaderMap::new();
        headers.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        headers.insert(
            "x-pilot-csrf",
            HeaderValue::from_str(view["csrf"].as_str().unwrap()).unwrap(),
        );
        let session = pilot.authorize(&headers, Role::Operator, false).unwrap();
        let subscription = pilot.subscribe(session, Role::Operator).unwrap();
        let source_id = pilot
            .start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja)
            .unwrap();
        let live_id = pilot
            .start_route_with_mode(
                1,
                Source::RemoteMix,
                Route::Live,
                Language::En,
                Some(LiveMode::Interpretation),
            )
            .unwrap();
        Self {
            pilot,
            backend,
            subscription,
            windows,
            opens,
            closes,
            source_id,
            live_id,
        }
    }
    async fn pair(&mut self) -> (Window, Window) {
        let a = tokio::time::timeout(Duration::from_secs(1), self.windows.recv())
            .await
            .unwrap()
            .unwrap();
        let b = tokio::time::timeout(Duration::from_secs(1), self.windows.recv())
            .await
            .unwrap()
            .unwrap();
        assert_ne!(a.route, b.route);
        if a.route == Route::Transcription {
            (a, b)
        } else {
            (b, a)
        }
    }
    async fn next(&mut self) -> Value {
        let event = tokio::time::timeout(Duration::from_secs(1), self.subscription.events.recv())
            .await
            .unwrap()
            .expect("operator event");
        assert_ne!(event["type"], "gap", "unexpected runtime gap: {event}");
        assert_ne!(event["type"], "stopped");
        event
    }
    async fn windows(&mut self, state: &str, window: u64) {
        let mut routes = Vec::new();
        for _ in 0..8 {
            let event = self.next().await;
            if event["type"] == "window" {
                assert_eq!(event["state"], state);
                assert_eq!(event["window"], window);
                assert!(event["route_id"] == self.source_id || event["route_id"] == self.live_id);
                assert!(!routes.contains(&event["route_id"]));
                routes.push(event["route_id"].clone());
                if routes.len() == 2 {
                    return;
                }
            } else {
                assert_eq!(event["type"], "route");
            }
        }
        panic!("both route window notices must arrive");
    }
    fn frame(&self, sequence: u32) {
        let mut data = vec![0; 646];
        data[0] = 1;
        data[2..6].copy_from_slice(&sequence.to_le_bytes());
        self.pilot.frame(&data).unwrap();
    }
    fn finish(&self) {
        self.pilot
            .finish_from_socket(1, &self.subscription.id, &self.subscription.session)
            .unwrap();
    }
}

#[tokio::test(start_paused = true)]
async fn two_routes_renew_then_finish_cancels_live_and_preserves_final_source_window() {
    let mut rig = Rig::new();
    let (mut source, mut live) = rig.pair().await;
    source.ready().await;
    live.ready().await;
    rig.windows("ready", 1).await;
    rig.frame(0);
    assert!(matches!(source.next_input().await, StreamInput::Pcm16(_)));
    assert!(matches!(live.next_input().await, StreamInput::Pcm16(_)));
    source.source(1, false, "架空の").await;
    let draft = rig.next().await;
    assert_eq!(draft["window"], 1);
    assert_eq!(draft["provenance"], "source_asr");
    live.events
        .send(StreamEvent::Audio {
            pcm16: vec![0; 640],
            sample_rate: 16000,
        })
        .await
        .unwrap();
    let audio = rig.next().await;
    assert_eq!(audio["type"], "audio");
    assert_eq!(audio["window"], 1);

    tokio::time::advance(Duration::from_secs(5)).await;
    rig.windows("draining", 1).await;
    assert!(matches!(
        source.next_input().await,
        StreamInput::FinishAtBoundary
    ));
    assert!(matches!(live.next_input().await, StreamInput::FinishInput));
    // Native source input remains open until its own natural boundary receipt.
    rig.frame(1);
    assert!(matches!(source.next_input().await, StreamInput::Pcm16(_)));
    source.source(2, true, "架空の会話です。").await;
    let final_old = rig.next().await;
    assert_eq!(final_old["window"], 1);
    assert_eq!(final_old["final"], true);
    assert_eq!(final_old["item"]["revision"], 2);
    source.complete_planned();
    live.complete_planned();
    rig.windows("renewing", 2).await;
    let (mut next_source, mut next_live) = rig.pair().await;
    assert_eq!(rig.opens.load(Ordering::SeqCst), 4);
    assert_eq!(rig.closes.load(Ordering::SeqCst), 2);
    assert!(*source.stop.borrow() && *live.stop.borrow());
    assert!(
        live.input.try_recv().is_err(),
        "sealed Live PCM must be discarded"
    );
    next_source.ready().await;
    next_live.ready().await;
    rig.windows("ready", 2).await;
    next_source.source(1, false, "次の").await;
    let draft_next = rig.next().await;
    assert_eq!(draft_next["window"], 2);
    assert_eq!(draft_next["item"]["id"], final_old["item"]["id"]);
    assert_eq!(draft_next["item"]["revision"], 1);
    rig.frame(2);
    assert!(matches!(
        next_source.next_input().await,
        StreamInput::Pcm16(_)
    ));
    assert!(matches!(
        next_live.next_input().await,
        StreamInput::Pcm16(_)
    ));

    rig.finish();
    assert!(*next_source.fence.borrow() && *next_live.fence.borrow());
    assert!(rig.pilot.is_finishing() && !rig.pilot.is_stopped());
    assert_eq!(rig.next().await["type"], "finishing");
    let live_closed = rig.next().await;
    assert_eq!(live_closed["route_id"], rig.live_id);
    assert_eq!(live_closed["state"], "closed");
    assert!(matches!(
        next_source.next_input().await,
        StreamInput::FinishInput
    ));
    tokio::task::yield_now().await;
    assert!(*next_live.stop.borrow());
    assert!(
        !*rig.backend.shared.stop.borrow(),
        "Live cancellation must not poison source drain"
    );
    next_source.source(2, true, "次の架空の文章です。").await;
    let final_next = rig.next().await;
    assert_eq!(final_next["type"], "transcript");
    assert_eq!(final_next["window"], 2);
    assert_eq!(final_next["final"], true);
    next_source
        .events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    next_source
        .events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert_eq!(rig.next().await["state"], "closed");
    let finished = rig.next().await;
    assert_eq!(finished["type"], "finished");
    assert_eq!(finished["source_completion"], "complete");
    assert!(rig.subscription.events.try_recv().is_err());
    assert!(!rig.pilot.is_stopped());
    rig.pilot.stop("operator_stop");
    rig.backend.shutdown().await;
    assert_eq!(rig.opens.load(Ordering::SeqCst), 4);
    assert_eq!(rig.closes.load(Ordering::SeqCst), 4);
    assert!(matches!(
        rig.pilot.positive(rig.pilot.generation()),
        Err(ApiError::Stopped)
    ));
}

#[tokio::test(start_paused = true)]
async fn immediate_stop_fences_both_queued_confirmed_handovers_before_factory_admission() {
    let mut rig = Rig::new();
    let (mut source, mut live) = rig.pair().await;
    source.ready().await;
    live.ready().await;
    rig.windows("ready", 1).await;
    tokio::time::advance(Duration::from_secs(5)).await;
    rig.windows("draining", 1).await;
    assert!(matches!(
        source.next_input().await,
        StreamInput::FinishAtBoundary
    ));
    assert!(matches!(live.next_input().await, StreamInput::FinishInput));
    // Ready completions are queued with no intervening yield. Stop must beat
    // those receipts and prohibit both new factories, despite confirmed close.
    source.complete_planned();
    live.complete_planned();
    rig.pilot.stop("operator_stop");
    rig.backend.shutdown().await;
    assert!(rig.pilot.is_stopped());
    assert!(*source.stop.borrow() && *live.stop.borrow());
    assert_eq!(rig.opens.load(Ordering::SeqCst), 2);
    assert_eq!(rig.closes.load(Ordering::SeqCst), 2);
    assert!(rig.windows.try_recv().is_err());
    while let Ok(event) = rig.subscription.events.try_recv() {
        assert!(!rig.pilot.event_current(Role::Operator, &event));
    }
    assert!(matches!(
        rig.pilot.start_route(
            rig.pilot.generation(),
            Source::RemoteMix,
            Route::Transcription,
            Language::Ja
        ),
        Err(ApiError::Stopped)
    ));
}
