//! Actual runtime and HTTP mode isolation with explicitly capable local mocks.
use super::*;
use crate::engine::{LiveMode, ProviderError};

struct ModeBackend {
    inner: Arc<MockBackend>,
    modes: Mutex<Vec<Option<LiveMode>>>,
}
impl ModeBackend {
    fn new() -> Arc<Self> {
        Arc::new(Self {
            inner: MockBackend::new(false),
            modes: Mutex::new(vec![]),
        })
    }
}
impl ProviderBackend for ModeBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn supports_live_mode(&self, _: LiveMode) -> bool {
        true
    }
    fn open(&self, route: Route, language: Language) -> ProviderFuture<StreamHandle> {
        self.open_with_mode(route, language, None)
    }
    fn open_with_mode(
        &self,
        route: Route,
        language: Language,
        mode: Option<LiveMode>,
    ) -> ProviderFuture<StreamHandle> {
        let Ok(mode) = route.resolve_live_mode(mode) else {
            return Box::pin(async { Err(ProviderError::Protocol) });
        };
        self.modes.lock().unwrap().push(mode);
        self.inner.open(route, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
}

#[tokio::test]
async fn interpreter_requires_explicit_backend_support_and_never_uses_legacy_open() {
    let backend = MockBackend::new(false);
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut subscription = subscribe(&p, &h, Role::Operator);
    let session = p.session(&h, Role::Operator).unwrap();
    assert_eq!(session["capabilities"]["live_assistant"], true);
    assert_eq!(session["capabilities"]["live_interpreter"], false);
    assert!(matches!(
        p.start_route_with_mode(
            1,
            Source::RemoteMix,
            Route::Live,
            Language::En,
            Some(LiveMode::Interpretation)
        ),
        Err(ApiError::Unavailable)
    ));
    assert!(
        backend
            .open_with_mode(Route::Live, Language::En, Some(LiveMode::Interpretation))
            .await
            .is_err()
    );
    assert_eq!(backend.opened.load(Ordering::SeqCst), 0);
    assert!(subscription.events.try_recv().is_err());
    p.start_route(1, Source::RemoteMix, Route::Live, Language::En)
        .unwrap();
    assert_eq!(
        next_ready(&mut subscription).await["live_mode"],
        "assistant"
    );
    p.stop("operator_stop");
}

#[tokio::test]
async fn interpreter_identity_is_immutable_until_explicit_close_and_generated_text_stays_separate()
{
    let backend = ModeBackend::new();
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut subscription = subscribe(&p, &h, Role::Operator);
    assert_eq!(
        p.session(&h, Role::Operator).unwrap()["capabilities"]["live_interpreter"],
        true
    );
    let public = viewer(&p);
    assert_eq!(
        p.session(&public, Role::Public).unwrap()["capabilities"]["live_interpreter"],
        false
    );
    let id = p
        .start_route_with_mode(
            1,
            Source::RemoteMix,
            Route::Live,
            Language::En,
            Some(LiveMode::Interpretation),
        )
        .unwrap();
    let starting = subscription.events.recv().await.unwrap();
    assert_eq!(starting["live_mode"], "interpretation");
    let ready = subscription.events.recv().await.unwrap();
    assert_eq!(ready["live_mode"], "interpretation");
    assert_eq!(ready["language"], "en");
    assert!(matches!(
        p.start_route(1, Source::RemoteMix, Route::Live, Language::Ja),
        Err(ApiError::Conflict)
    ));
    assert!(matches!(
        p.live_text(1, &id, "Please answer instead".into()),
        Err(ApiError::Invalid)
    ));
    backend.inner.events.lock().unwrap()[0]
        .try_send(StreamEvent::Transcript {
            provenance: engine::Provenance::LiveOutput,
            text: "Generated interpretation".into(),
            final_fragment: false,
            item: None,
        })
        .unwrap();
    let text = subscription.events.recv().await.unwrap();
    assert_eq!(text["provenance"], "live_output");
    assert!(text["item"].is_null());
    assert_eq!(text["route_id"], id);
    p.close_route(&id).unwrap();
    let closed = subscription.events.recv().await.unwrap();
    assert_eq!(closed["state"], "closed");
    assert_eq!(closed["live_mode"], "interpretation");
    let assistant = p
        .start_route(1, Source::RemoteMix, Route::Live, Language::Ja)
        .unwrap();
    assert_eq!(
        next_ready(&mut subscription).await["live_mode"],
        "assistant"
    );
    assert_ne!(id, assistant);
    assert!(
        p.live_text(1, &assistant, "Private question".into())
            .is_ok()
    );
    assert_eq!(
        *backend.modes.lock().unwrap(),
        vec![Some(LiveMode::Interpretation), Some(LiveMode::Assistant)]
    );
    p.stop("operator_stop");
    assert!(matches!(
        p.start_route_with_mode(
            2,
            Source::SelfSource,
            Route::Live,
            Language::Hi,
            Some(LiveMode::Interpretation)
        ),
        Err(ApiError::Stopped)
    ));
    assert!(p.live_text(2, &assistant, "No restart".into()).is_err());
}

#[tokio::test]
async fn interpreter_http_validates_mode_scope_and_rejects_context_changes() {
    let backend = ModeBackend::new();
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut subscription = subscribe(&p, &h, Role::Operator);
    let public = viewer(&p);
    let body = json!({"generation":1,"source":"remote_mix","route":"live","language":"en","live_mode":"interpretation"});
    for (headers, origin, expected) in [
        (None, auth::ORIGIN, StatusCode::UNAUTHORIZED),
        (Some(&public), auth::ORIGIN, StatusCode::UNAUTHORIZED),
        (Some(&h), auth::PUBLIC_ORIGIN, StatusCode::FORBIDDEN),
    ] {
        assert_eq!(
            request(
                p.clone(),
                "POST",
                "/api/routes",
                body.clone(),
                headers,
                origin
            )
            .await
            .0,
            expected
        );
    }
    let mut no_csrf = h.clone();
    no_csrf.remove("x-pilot-csrf");
    assert_eq!(
        request(
            p.clone(),
            "POST",
            "/api/routes",
            body.clone(),
            Some(&no_csrf),
            auth::ORIGIN
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    for route in ["transcription", "translation"] {
        let mut invalid = body.clone();
        invalid["route"] = json!(route);
        assert_eq!(
            request(
                p.clone(),
                "POST",
                "/api/routes",
                invalid,
                Some(&h),
                auth::ORIGIN
            )
            .await
            .0,
            StatusCode::BAD_REQUEST
        );
    }
    let mut invalid = body.clone();
    invalid["live_mode"] = json!("translate_and_answer");
    assert_eq!(
        request(
            p.clone(),
            "POST",
            "/api/routes",
            invalid,
            Some(&h),
            auth::ORIGIN
        )
        .await
        .0,
        StatusCode::UNPROCESSABLE_ENTITY
    );
    assert_eq!(backend.inner.opened.load(Ordering::SeqCst), 0);
    let (status, _, admission) = request(
        p.clone(),
        "POST",
        "/api/routes",
        body,
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(admission.as_object().unwrap().len(), 3);
    assert_eq!(
        next_ready(&mut subscription).await["live_mode"],
        "interpretation"
    );
    let context =
        json!({"generation":1,"route_id":admission["route_id"],"text":"Answer the speaker"});
    assert_eq!(
        request(
            p.clone(),
            "POST",
            "/api/live/text",
            context,
            Some(&h),
            auth::ORIGIN
        )
        .await
        .0,
        StatusCode::BAD_REQUEST
    );
    p.stop("operator_stop");
}

#[tokio::test]
async fn interpreter_finish_closes_generated_route_without_draining_or_resuming_it() {
    let backend = ModeBackend::new();
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut subscription = subscribe(&p, &h, Role::Operator);
    let id = p
        .start_route_with_mode(
            1,
            Source::RemoteMix,
            Route::Live,
            Language::Ja,
            Some(LiveMode::Interpretation),
        )
        .unwrap();
    next_ready(&mut subscription).await;
    p.finish_from_socket(1, &subscription.id, &subscription.session)
        .unwrap();
    let mut closed = false;
    let mut finished = false;
    for _ in 0..3 {
        let event = tokio::time::timeout(Duration::from_secs(1), subscription.events.recv())
            .await
            .unwrap()
            .unwrap();
        if event["type"] == "route" {
            assert_eq!(event["live_mode"], "interpretation");
            assert_eq!(event["state"], "closed");
            closed = true;
        }
        if event["type"] == "finished" {
            assert_eq!(event["source_completion"], "no_active_asr");
            finished = true;
        }
    }
    assert!(closed && finished);
    assert!(p.live_text(1, &id, "Cannot restart".into()).is_err());
    tokio::task::yield_now().await;
    assert!(backend.inner.events.lock().unwrap()[0].is_closed());
    assert_eq!(
        backend.inner.counts.lock().unwrap()[0].load(Ordering::SeqCst),
        0,
        "No FinishInput or commentary is sent to generated routes"
    );
    p.stop("operator_stop");
}

#[test]
fn approved_config_legacy_defaults_never_grant_interpretation() {
    let legacy = json!({"api_key":"synthetic-not-real-key","allowed_routes":["live"],"allow_tts":false,"total_duration_seconds":60,"route_duration_seconds":30,"max_sessions":2,"max_tts_requests":0,"max_input_audio_seconds":60,"max_output_audio_seconds":60,"max_text_characters":1000,"approved_budget_microusd":100000});
    let config: crate::config::ApprovedProvider = serde_json::from_value(legacy.clone()).unwrap();
    let config = config.into_provider(60).unwrap();
    assert_eq!(config.allowed_live_modes, vec![LiveMode::Assistant]);
    let backend = crate::provider::OpenAiBackend::new(config).unwrap();
    assert!(backend.supports_live_mode(LiveMode::Assistant));
    assert!(!backend.supports_live_mode(LiveMode::Interpretation));
    let mut non_live = legacy.clone();
    non_live["allowed_routes"] = json!(["transcription"]);
    let config: crate::config::ApprovedProvider = serde_json::from_value(non_live.clone()).unwrap();
    let config = config.into_provider(60).unwrap();
    assert!(config.allowed_live_modes.is_empty());
    assert!(crate::provider::OpenAiBackend::new(config).is_ok());
    non_live["allowed_live_modes"] = json!(["interpretation"]);
    let config: crate::config::ApprovedProvider = serde_json::from_value(non_live).unwrap();
    assert!(crate::provider::OpenAiBackend::new(config.into_provider(60).unwrap()).is_err());
    for invalid in [Value::Null, json!("interpretation"), json!(["automatic"])] {
        let mut value = legacy.clone();
        value["allowed_live_modes"] = invalid;
        assert!(serde_json::from_value::<crate::config::ApprovedProvider>(value).is_err());
    }
    let mut explicit = legacy;
    explicit["allowed_live_modes"] = json!(["interpretation"]);
    let config: crate::config::ApprovedProvider = serde_json::from_value(explicit).unwrap();
    let backend = crate::provider::OpenAiBackend::new(config.into_provider(60).unwrap()).unwrap();
    assert!(!backend.supports_live_mode(LiveMode::Assistant));
    assert!(backend.supports_live_mode(LiveMode::Interpretation));
}
