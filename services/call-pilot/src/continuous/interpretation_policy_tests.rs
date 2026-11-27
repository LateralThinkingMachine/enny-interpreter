//! Actual supervisor propagation and cancellation with synthetic owned children.
use super::*;
use crate::engine::Interpretation;
use crate::interpretation_context::{
    InterpretationContext, InterpretationTerm, InterpretationVoice,
};
use serde_json::json;
struct Interpreter {
    inner: Arc<Mock>,
    seen: Arc<Mutex<Vec<Interpretation>>>,
    contexts: Arc<Mutex<Vec<InterpretationContext>>>,
}
impl ProviderBackend for Interpreter {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
    fn open_interpretation(
        &self,
        language: Language,
        policy: Interpretation,
    ) -> ProviderFuture<StreamHandle> {
        self.open_interpretation_context(language, policy, Default::default())
    }
    fn open_interpretation_context(
        &self,
        language: Language,
        policy: Interpretation,
        context: InterpretationContext,
    ) -> ProviderFuture<StreamHandle> {
        policy.validate(language).unwrap();
        self.seen.lock().unwrap().push(policy);
        self.contexts.lock().unwrap().push(context);
        self.inner.open(Route::Live, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}
struct InterpreterOwner(Arc<Interpreter>);
impl WindowOwner for InterpreterOwner {
    fn backend(&self) -> Arc<dyn ProviderBackend> {
        self.0.clone()
    }
    fn shutdown(&self) -> Shutdown<'_> {
        Box::pin(async {
            Retirement {
                released: true,
                confirmed: true,
                remote_closed: true,
            }
        })
    }
}
async fn context_renewal(initial: InterpretationContext) {
    let (tx, mut windows) = mpsc::unbounded_channel();
    let seen = Arc::new(Mutex::new(vec![]));
    let records = seen.clone();
    let contexts = Arc::new(Mutex::new(vec![]));
    let observed_contexts = contexts.clone();
    let factory: Factory = Arc::new(move |route, _, _| {
        assert_eq!(route, Route::Live);
        let (input, input_rx) = mpsc::channel(8);
        let (events, event_rx) = mpsc::channel(32);
        let (stop, stop_rx) = watch::channel(false);
        tx.send(Witness {
            events,
            input: input_rx,
            stop: stop_rx,
        })
        .unwrap();
        Ok(Arc::new(InterpreterOwner(Arc::new(Interpreter {
            inner: Arc::new(Mock {
                handle: Mutex::new(Some(StreamHandle {
                    input,
                    events: event_rx,
                    stop,
                    renewal_fence: None,
                })),
                closes: Arc::new(AtomicUsize::new(0)),
                delay: Duration::ZERO,
                certain: true,
            }),
            seen: records.clone(),
            contexts: observed_contexts.clone(),
        }))))
    });
    let backend = ContinuousBackend::with_factory(policy(), factory);
    let pair: Interpretation = serde_json::from_value(
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"ja","second":"en"}),
    )
    .unwrap();
    let mut route = backend
        .open_interpretation_context(Language::Ja, pair, initial.clone())
        .await
        .unwrap();
    let mut first = windows.recv().await.unwrap();
    ready(&first, &mut route).await;
    tokio::time::advance(Duration::from_secs(135)).await;
    assert!(matches!(
        route.events.recv().await,
        Some(StreamEvent::Handover)
    ));
    assert!(matches!(
        first.input.recv().await,
        Some(StreamInput::FinishInput)
    ));
    first
        .events
        .send(StreamEvent::Drained {
            completion: DrainCompletion::ProviderSessionClosed,
        })
        .await
        .unwrap();
    first
        .events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert!(matches!(
        route.events.recv().await,
        Some(StreamEvent::Renewing { window: 2 })
    ));
    let second = windows.recv().await.unwrap();
    ready(&second, &mut route).await;
    second
        .events
        .send(StreamEvent::Error(ProviderError::ConnectionLost))
        .await
        .unwrap();
    assert!(matches!(
        route.events.recv().await,
        Some(StreamEvent::Recovering { window: 3, .. })
    ));
    let third = windows.recv().await.unwrap();
    ready(&third, &mut route).await;
    assert_eq!(seen.lock().unwrap().as_slice(), &[pair, pair, pair]);
    assert_eq!(
        contexts.lock().unwrap().as_slice(),
        &[initial.clone(), initial.clone(), initial]
    );
    assert_eq!(backend.shared.ledger.lock().unwrap().windows, 3);
    let allocated = backend
        .budget_snapshot()
        .continuous
        .unwrap()
        .allocated_microusd;
    backend.revoke();
    assert!(
        backend
            .open_interpretation(Language::Ja, pair)
            .await
            .is_err()
    );
    assert!(backend.retire().await);
    assert!(backend.closure_confirmed());
    tokio::time::advance(Duration::from_secs(1000)).await;
    assert!(windows.try_recv().is_err());
    assert_eq!(
        backend
            .budget_snapshot()
            .continuous
            .unwrap()
            .allocated_microusd,
        allocated
    );
}

#[tokio::test(start_paused = true)]
async fn pair_policy_survives_renewal_and_recovery_then_revocation_prevents_new_windows() {
    context_renewal(Default::default()).await;
}
#[tokio::test(start_paused = true)]
async fn literal_context_and_selected_voice_survive_renewal_recovery_and_stop() {
    context_renewal(InterpretationContext {
        topic: "Budget meeting".into(),
        terms: vec![InterpretationTerm {
            source: "青葉".into(),
            preferred: "Aoba".into(),
            pronunciation: "あおば".into(),
        }],
        voice: InterpretationVoice::Gleam,
    })
    .await;
}
