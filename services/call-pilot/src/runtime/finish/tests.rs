use super::*;
use crate::engine::{
    DrainCompletion, PreparedAudio, Provenance, ProviderError, ProviderFuture, TranscriptItem,
};

mod transport;

struct ChannelBackend {
    ready: bool,
    inputs: Mutex<Vec<mpsc::Receiver<StreamInput>>>,
    events: Mutex<Vec<mpsc::Sender<StreamEvent>>>,
    stops: Mutex<Vec<watch::Receiver<bool>>>,
}
impl ChannelBackend {
    fn new(ready: bool) -> Arc<Self> {
        Arc::new(Self {
            ready,
            inputs: Mutex::new(vec![]),
            events: Mutex::new(vec![]),
            stops: Mutex::new(vec![]),
        })
    }
}
impl ProviderBackend for ChannelBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        let (input, received) = mpsc::channel(8);
        let (events, receiver) = mpsc::channel(32);
        let (stop, stopped) = watch::channel(false);
        self.inputs.lock().unwrap().push(received);
        self.events.lock().unwrap().push(events.clone());
        self.stops.lock().unwrap().push(stopped);
        let ready = self.ready;
        Box::pin(async move {
            if ready {
                events
                    .send(StreamEvent::Ready {
                        expires_in_seconds: 50,
                    })
                    .await
                    .unwrap();
            }
            Ok(StreamHandle {
                renewal_fence: None,
                input,
                events: receiver,
                stop,
            })
        })
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
}
fn setup(ready: bool) -> (Arc<Pilot>, Arc<ChannelBackend>, Subscription) {
    let backend = ChannelBackend::new(ready);
    let (pilot, ticket) = Pilot::new(backend.clone(), Duration::from_secs(60), 30).unwrap();
    pilot.redeem(&ticket, Role::Operator).unwrap();
    let session = pilot
        .memory
        .lock()
        .unwrap()
        .operator
        .as_ref()
        .unwrap()
        .id
        .clone();
    let subscription = pilot.subscribe(session, Role::Operator).unwrap();
    (pilot, backend, subscription)
}
async fn event(subscription: &mut Subscription) -> Value {
    tokio::time::timeout(Duration::from_secs(1), subscription.events.recv())
        .await
        .unwrap()
        .unwrap()
}
async fn open(
    p: &Arc<Pilot>,
    subscription: &mut Subscription,
    route: Route,
    ready: bool,
) -> String {
    let id = p
        .start_route(1, Source::RemoteMix, route, Language::Ja)
        .unwrap();
    assert_eq!(event(subscription).await["state"], "starting");
    if ready {
        assert_eq!(event(subscription).await["state"], "ready");
    } else {
        tokio::task::yield_now().await;
    }
    id
}
fn frame(sequence: u32) -> Vec<u8> {
    let mut value = vec![1, 0];
    value.extend(sequence.to_le_bytes());
    value.extend(vec![sequence as u8; 640]);
    value
}
fn final_item() -> StreamEvent {
    StreamEvent::Transcript {
        provenance: Provenance::SourceAsr,
        text: "The last accepted phrase.".into(),
        final_fragment: true,
        item: Some(TranscriptItem {
            source_audio: None,
            id: "source_item".into(),
            revision: 1,
            sequence: 1,
        }),
    }
}
async fn finish_received(b: &ChannelBackend) -> mpsc::Receiver<StreamInput> {
    let mut input = b.inputs.lock().unwrap().remove(0);
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), input.recv())
            .await
            .unwrap(),
        Some(StreamInput::FinishInput)
    ));
    input
}

#[tokio::test]
async fn finish_follows_full_accepted_fifo_and_final_precedes_closed_and_marker() {
    let (p, b, mut s) = setup(true);
    let id = open(&p, &mut s, Route::Transcription, true).await;
    for n in 0..8 {
        p.frame(&frame(n)).unwrap();
    }
    p.finish_from_socket(1, &s.id, &s.session).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    assert!(matches!(p.finish(1), Err(ApiError::Conflict)));
    assert!(p.frame(&frame(8)).is_err());
    assert!(
        p.start_route(1, Source::SelfSource, Route::Live, Language::En)
            .is_err()
    );
    assert!(p.public_ticket(1).is_err());
    assert!(
        p.prepare(1, "must not start".into(), Language::En)
            .await
            .is_err()
    );
    let mut input = b.inputs.lock().unwrap().remove(0);
    for n in 0..8 {
        let Some(StreamInput::Pcm16(bytes)) = input.recv().await else {
            panic!("Finish preceded accepted PCM");
        };
        assert_eq!(bytes, vec![n as u8; 640]);
    }
    assert!(matches!(input.recv().await, Some(StreamInput::FinishInput)));
    let events = b.events.lock().unwrap()[0].clone();
    events.send(final_item()).await.unwrap();
    events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    let final_event = event(&mut s).await;
    assert_eq!(final_event["type"], "transcript");
    assert_eq!(final_event["final"], true);
    let closed = event(&mut s).await;
    assert_eq!(closed["route_id"], id);
    assert_eq!(closed["state"], "closed");
    assert_eq!(
        event(&mut s).await,
        json!({"type":"finished","generation":1,"source_completion":"complete"})
    );
    assert!(!p.is_stopped());
    p.heartbeat(&s.id, Role::Operator, 1).unwrap();
    assert!(p.connection_current(&s.id, &s.session, Role::Operator));
    assert!(p.positive(1).is_err());
    assert!(input.recv().await.is_none());
    p.stop("operator_stop");
}

#[tokio::test]
async fn finish_immediately_revokes_generated_routes_and_public_capabilities() {
    let (p, b, mut s) = setup(true);
    let ticket = p.public_ticket(1).unwrap();
    p.redeem(ticket["ticket"].as_str().unwrap(), Role::Public)
        .unwrap();
    let viewer = p.memory.lock().unwrap().viewer.as_ref().unwrap().id.clone();
    let public = p.subscribe(viewer, Role::Public).unwrap();
    let id = open(&p, &mut s, Route::Translation, true).await;
    p.finish_from_socket(1, &s.id, &s.session).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    assert!(*b.stops.lock().unwrap()[0].borrow());
    assert!(!p.connection_current(&public.id, &public.session, Role::Public));
    assert!(p.live_text(1, &id, "no output".into()).is_err());
    assert_eq!(event(&mut s).await["state"], "closed");
    assert_eq!(event(&mut s).await["source_completion"], "no_active_asr");
    assert!(b.inputs.lock().unwrap()[0].try_recv().is_err());
    p.stop("operator_stop");
}

#[tokio::test]
async fn starting_asr_is_incomplete_and_is_never_promoted_by_late_ready() {
    let (p, b, mut s) = setup(false);
    open(&p, &mut s, Route::Transcription, false).await;
    p.finish_from_socket(1, &s.id, &s.session).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    assert_eq!(event(&mut s).await["state"], "closed");
    assert_eq!(event(&mut s).await["source_completion"], "incomplete");
    assert!(*b.stops.lock().unwrap()[0].borrow());
    assert!(b.inputs.lock().unwrap()[0].try_recv().is_err());
    p.stop("operator_stop");
}

#[tokio::test]
async fn missing_wrong_duplicate_or_unsealed_drain_and_unknown_close_are_incomplete() {
    for case in 0..6 {
        let (p, b, mut s) = setup(true);
        open(&p, &mut s, Route::Transcription, true).await;
        p.finish_from_socket(1, &s.id, &s.session).unwrap();
        assert_eq!(event(&mut s).await["type"], "finishing");
        let _input = finish_received(&b).await;
        let events = b.events.lock().unwrap()[0].clone();
        let sequence = match case {
            0 => vec![StreamEvent::Closed { confirmed: true }],
            1 => vec![StreamEvent::Drained {
                completion: DrainCompletion::ObservationWindowEnded,
            }],
            2 => vec![
                StreamEvent::Drained {
                    completion: DrainCompletion::CommittedTranscriptsFinal,
                },
                StreamEvent::Closed { confirmed: false },
            ],
            3 => vec![
                StreamEvent::Drained {
                    completion: DrainCompletion::CommittedTranscriptsFinal,
                },
                StreamEvent::Drained {
                    completion: DrainCompletion::CommittedTranscriptsFinal,
                },
            ],
            4 => vec![
                StreamEvent::Transcript {
                    provenance: Provenance::SourceAsr,
                    text: "still interim".into(),
                    final_fragment: false,
                    item: Some(TranscriptItem {
                        source_audio: None,
                        id: "item".into(),
                        revision: 1,
                        sequence: 1,
                    }),
                },
                StreamEvent::Drained {
                    completion: DrainCompletion::CommittedTranscriptsFinal,
                },
            ],
            _ => vec![StreamEvent::Drained {
                completion: DrainCompletion::ProviderSessionClosed,
            }],
        };
        for value in sequence {
            let _ = events.send(value).await;
        }
        loop {
            let value = event(&mut s).await;
            if value["type"] == "finished" {
                assert_eq!(value["source_completion"], "incomplete", "case {case}");
                break;
            }
        }
        p.stop("operator_stop");
    }
}

#[tokio::test]
async fn bounded_timeout_retires_full_fifo_without_late_finish_or_final() {
    let (p, b, mut s) = setup(true);
    open(&p, &mut s, Route::Transcription, true).await;
    for n in 0..8 {
        p.frame(&frame(n)).unwrap();
    }
    let began = Instant::now();
    p.begin_finish(1, Duration::from_millis(30), Some((&s.id, &s.session)))
        .unwrap();
    // Stall the current-thread timer as well as its waiter. std::Instant must
    // deny resumed input/output even before Tokio observes timer readiness.
    std::thread::sleep(Duration::from_millis(45));
    let events = b.events.lock().unwrap()[0].clone();
    let _ = events.send(final_item()).await;
    loop {
        let value = event(&mut s).await;
        assert_ne!(value["type"], "transcript");
        if value["type"] == "finished" {
            assert_eq!(value["source_completion"], "incomplete");
            break;
        }
    }
    assert!(began.elapsed() < Duration::from_secs(1));
    let mut input = b.inputs.lock().unwrap().remove(0);
    for _ in 0..8 {
        assert!(matches!(input.recv().await, Some(StreamInput::Pcm16(_))));
    }
    assert!(input.recv().await.is_none());
    assert!(*b.stops.lock().unwrap()[0].borrow());
    p.stop("operator_stop");
}

#[tokio::test]
async fn stop_logout_and_disconnect_supersede_finish_without_export_marker() {
    for action in 0..3 {
        let (p, b, mut s) = setup(true);
        open(&p, &mut s, Route::Transcription, true).await;
        for n in 0..8 {
            p.frame(&frame(n)).unwrap();
        }
        p.finish_from_socket(1, &s.id, &s.session).unwrap();
        assert_eq!(event(&mut s).await["type"], "finishing");
        match action {
            0 => p.stop("operator_stop"),
            1 => p.logout(Role::Operator),
            _ => p.disconnected(&s.id, Role::Operator),
        }
        tokio::task::yield_now().await;
        assert!(p.is_stopped());
        while let Ok(value) = s.events.try_recv() {
            assert_ne!(value["type"], "finished");
        }
        let mut input = b.inputs.lock().unwrap().remove(0);
        for _ in 0..8 {
            assert!(matches!(input.recv().await, Some(StreamInput::Pcm16(_))));
        }
        assert!(input.recv().await.is_none());
    }
}

#[tokio::test]
async fn source_gap_is_not_erased_by_a_finish_with_no_active_asr() {
    let (p, _, mut s) = setup(true);
    p.source_gap(Source::RemoteMix, "sequence_gap");
    assert_eq!(event(&mut s).await["type"], "gap");
    p.finish_from_socket(1, &s.id, &s.session).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    assert_eq!(event(&mut s).await["source_completion"], "incomplete");
    p.stop("operator_stop");
}

#[tokio::test]
async fn fallback_without_socket_ordering_is_incomplete_and_foreign_socket_cannot_finish() {
    let (p, b, mut s) = setup(true);
    open(&p, &mut s, Route::Transcription, true).await;
    assert!(
        p.finish_from_socket(1, "other_subscription", &s.session)
            .is_err()
    );
    assert!(p.finish_from_socket(1, &s.id, "other_session").is_err());
    assert!(p.positive(1).is_ok());
    p.finish(1).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    let _input = finish_received(&b).await;
    let events = b.events.lock().unwrap()[0].clone();
    events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert_eq!(event(&mut s).await["state"], "closed");
    assert_eq!(event(&mut s).await["source_completion"], "incomplete");
    p.stop("operator_stop");
}

#[tokio::test]
async fn queued_generated_content_loses_egress_authority_but_source_finals_remain_observable() {
    let (p, _, mut s) = setup(true);
    let generated = [
        json!({"type":"audio"}),
        json!({"type":"preview"}),
        json!({"type":"transcript","provenance":"live_output"}),
        json!({"type":"transcript","provenance":"translation"}),
    ];
    for value in &generated {
        assert!(p.event_current(Role::Operator, value));
    }
    p.finish_from_socket(1, &s.id, &s.session).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    for value in &generated {
        assert!(!p.event_current(Role::Operator, value));
    }
    assert!(!p.event_current(Role::Public, &json!({"type":"delivery"})));
    assert!(p.event_current(
        Role::Operator,
        &json!({"type":"transcript","provenance":"source_asr"})
    ));
    assert!(p.event_current(Role::Operator, &json!({"type":"finished"})));
    p.stop("operator_stop");
    assert!(!p.event_current(Role::Operator, &json!({"type":"finished"})));
}

#[tokio::test]
async fn stale_generated_error_after_finish_does_not_cancel_surviving_asr() {
    let (p, b, mut s) = setup(true);
    let asr = open(&p, &mut s, Route::Transcription, true).await;
    let translation = open(&p, &mut s, Route::Translation, true).await;
    p.finish_from_socket(1, &s.id, &s.session).unwrap();
    assert_eq!(event(&mut s).await["type"], "finishing");
    assert_eq!(event(&mut s).await["route_id"], translation);
    // Models an Error already dequeued by consume_route before Finish won
    // the state lock. Its second, exact-route check must reject the callback.
    p.route_gap(&translation, Source::RemoteMix, "provider_closed");
    assert!(p.memory.lock().unwrap().routes.contains_key(&asr));
    let _input = finish_received(&b).await;
    let events = b.events.lock().unwrap()[0].clone();
    events.send(final_item()).await.unwrap();
    events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert_eq!(event(&mut s).await["type"], "transcript");
    assert_eq!(event(&mut s).await["route_id"], asr);
    assert_eq!(event(&mut s).await["source_completion"], "complete");
    p.stop("operator_stop");
}
