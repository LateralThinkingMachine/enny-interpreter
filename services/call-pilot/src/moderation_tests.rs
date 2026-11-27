//! Synthetic HTTP/runtime policy boundaries, not language quality acceptance.
use super::*;
use crate::engine::{Interpretation, LiveMode, RouteOptions, SpeechHints};

struct PolicyBackend {
    inner: Arc<MockBackend>,
    policies: Mutex<Vec<Interpretation>>,
    source_hints: Mutex<Vec<(Language, SpeechHints)>>,
    revoked: AtomicBool,
}
impl ProviderBackend for PolicyBackend {
    fn enabled(&self) -> bool {
        !self.revoked.load(Ordering::Acquire)
    }
    fn revoke(&self) {
        self.revoked.store(true, Ordering::Release);
    }
    fn supports_live_mode(&self, mode: LiveMode) -> bool {
        self.enabled() && mode == LiveMode::Interpretation
    }
    fn open(&self, route: Route, language: Language) -> ProviderFuture<StreamHandle> {
        self.inner.open(route, language)
    }
    fn open_interpretation(
        &self,
        language: Language,
        policy: Interpretation,
    ) -> ProviderFuture<StreamHandle> {
        self.policies.lock().unwrap().push(policy);
        self.inner.open(Route::Live, language)
    }
    fn open_transcription(
        &self,
        language: Language,
        hints: SpeechHints,
    ) -> ProviderFuture<StreamHandle> {
        self.source_hints.lock().unwrap().push((language, hints));
        self.inner.open(Route::Transcription, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
}
fn policy() -> Interpretation {
    serde_json::from_value(
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"ja","second":"en"}),
    )
    .unwrap()
}
fn backend() -> Arc<PolicyBackend> {
    Arc::new(PolicyBackend {
        inner: MockBackend::new(false),
        policies: Mutex::new(vec![]),
        source_hints: Mutex::new(vec![]),
        revoked: AtomicBool::new(false),
    })
}

#[tokio::test]
async fn expanded_source_anchor_and_interpreter_both_admit_through_http() {
    for (anchor, policy, languages) in [
        (
            Language::Vi,
            json!({"schema_version":"interpretation.v1","mode":"two_way","first":"vi","second":"es"}),
            vec![Language::Vi, Language::Es],
        ),
        (
            Language::Es,
            json!({"schema_version":"interpretation.v1","mode":"two_way","first":"es","second":"vi"}),
            vec![Language::Es, Language::Vi],
        ),
        (
            Language::Vi,
            json!({"schema_version":"interpretation.v1","mode":"auto_to_target","target":"vi"}),
            vec![
                Language::Ja,
                Language::En,
                Language::Hi,
                Language::Es,
                Language::Vi,
            ],
        ),
    ] {
        let b = backend();
        let (p, t) = hosted(b.clone());
        let h = operator(&p, &t);
        let mut events = subscribe(&p, &h, Role::Operator);
        let hints = SpeechHints {
            languages,
            keywords: vec![],
        };
        let (status, _, receipt) = request(p.clone(), "POST", "/api/routes",
            json!({"generation":1,"source":"remote_mix","route":"transcription","language":anchor,"transcription":hints}), Some(&h), auth::ORIGIN).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let ready = next_ready(&mut events).await;
        assert_eq!(ready["route_id"], receipt["route_id"]);
        assert_eq!(ready["language"], json!(anchor));
        assert!(ready.get("interpretation").is_none());
        assert!(ready.get("delivery").is_none());
        assert_eq!(events.events.recv().await.unwrap()["type"], "window");
        assert!(b.source_hints.lock().unwrap().as_slice() == [(anchor, hints)]);
        let (status, _, receipt) = request(p.clone(), "POST", "/api/routes",
            json!({"generation":1,"source":"remote_mix","route":"live","language":anchor,"live_mode":"interpretation","interpretation":policy}), Some(&h), auth::ORIGIN).await;
        assert_eq!(status, StatusCode::ACCEPTED);
        let ready = next_ready(&mut events).await;
        assert_eq!(ready["route_id"], receipt["route_id"]);
        assert_eq!(ready["language"], json!(anchor));
        assert_eq!(ready["interpretation"], policy);
        assert_eq!(b.inner.opened.load(Ordering::SeqCst), 2);
        p.stop("operator_stop");
    }
}

#[tokio::test]
async fn expanded_source_anchor_requires_explicit_matching_repeatable_continuous_scope() {
    for scope in ["finite", "one_use_continuous", "repeatable"] {
        let b = backend();
        let (p, t) = match scope {
            "repeatable" => hosted(b.clone()),
            "one_use_continuous" => {
                Pilot::new_persistent(b.clone(), 600, auth::LocalOrigins::default()).unwrap()
            }
            _ => pilot(b.clone()),
        };
        let h = operator(&p, &t);
        let mut events = subscribe(&p, &h, Role::Operator);
        for hints in [
            None,
            Some(json!({"languages":["ja","en","hi"],"keywords":[]})),
            Some(json!({"languages":["es"],"keywords":[]})),
        ] {
            let mut body = json!({"generation":1,"source":"remote_mix","route":"transcription","language":"vi"});
            if let Some(hints) = hints {
                body["transcription"] = hints;
            }
            let (status, _, _) = request(
                p.clone(),
                "POST",
                "/api/routes",
                body,
                Some(&h),
                auth::ORIGIN,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        if scope != "repeatable" {
            let (status, _, _) = request(p.clone(), "POST", "/api/routes",
                json!({"generation":1,"source":"remote_mix","route":"transcription","language":"vi","transcription":{"languages":["vi","es"],"keywords":[]}}), Some(&h), auth::ORIGIN).await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        assert_eq!(b.inner.opened.load(Ordering::SeqCst), 0);
        assert!(b.source_hints.lock().unwrap().is_empty());
        assert!(events.events.try_recv().is_err());
        p.stop("operator_stop");
    }
}
fn hosted(backend: Arc<dyn ProviderBackend>) -> (Arc<Pilot>, String) {
    Pilot::new_hosted(
        backend,
        None,
        600,
        auth::LocalOrigins::default(),
        1,
        true,
        std::time::Instant::now(),
    )
    .unwrap()
}

#[tokio::test]
async fn versioned_policy_binds_ready_audio_and_transcript_identity() {
    let b = backend();
    let (p, t) = hosted(b.clone());
    let h = operator(&p, &t);
    let mut events = subscribe(&p, &h, Role::Operator);
    let interpretation = policy();
    let body = json!({"generation":1,"source":"remote_mix","route":"live","language":"ja","live_mode":"interpretation","interpretation":interpretation});
    let (status, _, receipt) = request(
        p.clone(),
        "POST",
        "/api/routes",
        body,
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    let id = receipt["route_id"].as_str().unwrap();
    let ready = next_ready(&mut events).await;
    assert_eq!(ready["interpretation"], json!(interpretation));
    assert_eq!(b.policies.lock().unwrap().as_slice(), &[interpretation]);
    assert_eq!(events.events.recv().await.unwrap()["type"], "window");
    let provider = b.inner.events.lock().unwrap()[0].clone();
    provider
        .send(StreamEvent::Audio {
            pcm16: vec![0; 640],
            sample_rate: 16000,
        })
        .await
        .unwrap();
    let audio = events.events.recv().await.unwrap();
    assert_eq!(audio["route_id"], id);
    assert_eq!(audio["interpretation"], json!(interpretation));
    provider
        .send(StreamEvent::LiveTranscript {
            provenance: engine::Provenance::LiveOutput,
            text: "hello".into(),
            start_ms: 0.0,
            end_ms: 20.0,
        })
        .await
        .unwrap();
    let text = events.events.recv().await.unwrap();
    assert_eq!(text["interpretation"], json!(interpretation));
    assert!(text.get("detected_language").is_none());
    p.pause_processing(1).unwrap();
    assert!(b.revoked.load(Ordering::Acquire));
    assert!(
        p.start_route_with_options(
            1,
            Source::RemoteMix,
            Route::Live,
            Language::Ja,
            RouteOptions {
                live_mode: Some(LiveMode::Interpretation),
                interpretation: Some(interpretation),
                ..Default::default()
            }
        )
        .is_err()
    );
    assert!(!p.event_current(Role::Operator, &audio));
    p.stop("operator_stop");
}

#[tokio::test]
async fn finite_profiles_cannot_gain_new_scope() {
    let b = backend();
    let (finite, t) = pilot(b.clone());
    let h = operator(&finite, &t);
    let _events = subscribe(&finite, &h, Role::Operator);
    assert!(
        finite
            .start_route_with_options(
                1,
                Source::RemoteMix,
                Route::Live,
                Language::Ja,
                RouteOptions {
                    live_mode: Some(LiveMode::Interpretation),
                    interpretation: Some(policy()),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(
        finite
            .start_route_with_options(
                1,
                Source::RemoteMix,
                Route::Transcription,
                Language::En,
                RouteOptions {
                    transcription: Some(SpeechHints {
                        languages: vec![Language::Es, Language::Vi],
                        keywords: vec![]
                    }),
                    ..Default::default()
                }
            )
            .is_err()
    );
    assert!(
        finite
            .start_route(1, Source::RemoteMix, Route::Translation, Language::Vi)
            .is_err()
    );
    finite.stop("operator_stop");
    assert_eq!(b.inner.opened.load(Ordering::SeqCst), 0);
}

#[tokio::test]
async fn paused_control_keeps_only_existing_authority_and_a_started_frame() {
    let (p, t) = hosted(engine::disabled());
    let h = operator(&p, &t);
    let sub = subscribe(&p, &h, Role::Operator);
    let audio = json!({"type":"audio","generation":1});
    p.pause_processing(1).unwrap();
    assert!(!p.in_flight_event_current(Role::Operator, &audio, false));
    assert!(p.in_flight_event_current(Role::Operator, &audio, true));
    assert!(!p.in_flight_event_current(Role::Public, &audio, true));
    assert!(!p.in_flight_event_current(
        Role::Operator,
        &json!({"type":"audio","generation":2}),
        true
    ));
    p.mark_pause_retired(true);
    assert!(p.connection_current(&sub.id, &sub.session, Role::Operator));
    p.disconnected(&sub.id, Role::Operator);
    assert!(p.is_stopped());
    assert!(!p.in_flight_event_current(Role::Operator, &audio, true));
    assert!(p.heartbeat(&sub.id, Role::Operator, 1).is_err());
}

struct TtsScopeProbe(AtomicUsize);
impl ProviderBackend for TtsScopeProbe {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        self.0.fetch_add(1, Ordering::SeqCst);
        Box::pin(async { Ok(wav()) })
    }
}
#[tokio::test]
async fn expanded_interpreter_languages_never_expand_prepared_public_tts_scope() {
    for repeatable in [false, true] {
        let probe = Arc::new(TtsScopeProbe(AtomicUsize::new(0)));
        let (p, t) = if repeatable {
            hosted(probe.clone())
        } else {
            pilot(probe.clone())
        };
        let h = operator(&p, &t);
        let mut events = subscribe(&p, &h, Role::Operator);
        for language in [Language::Es, Language::Vi] {
            assert!(matches!(
                p.prepare(1, "Synthetic text".into(), language).await,
                Err(ApiError::Invalid)
            ));
            let (status, _, _) = request(
                p.clone(),
                "POST",
                "/api/previews",
                json!({"generation":1,"text":"Synthetic text","language":language}),
                Some(&h),
                auth::ORIGIN,
            )
            .await;
            assert_eq!(status, StatusCode::BAD_REQUEST);
        }
        assert_eq!(probe.0.load(Ordering::SeqCst), 0);
        assert!(events.events.try_recv().is_err());
        // Existing prepared speech retains its explicit supported scope.
        p.prepare(1, "Synthetic existing-language text".into(), Language::En)
            .await
            .unwrap();
        assert_eq!(probe.0.load(Ordering::SeqCst), 1);
        p.stop("operator_stop");
    }
}
