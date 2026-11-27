use super::*;
use crate::engine::{LiveMode, Provenance, RouteOptions, SpeechHints};

struct HintsBackend {
    inner: Arc<MockBackend>,
    hints: Mutex<Vec<SpeechHints>>,
}
impl ProviderBackend for HintsBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, route: Route, language: Language) -> ProviderFuture<StreamHandle> {
        self.inner.open(route, language)
    }
    fn open_transcription(
        &self,
        language: Language,
        hints: SpeechHints,
    ) -> ProviderFuture<StreamHandle> {
        self.hints.lock().unwrap().push(hints);
        self.inner.open(Route::Transcription, language)
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
}
fn hints() -> SpeechHints {
    SpeechHints {
        languages: vec![Language::Ja],
        keywords: vec!["佐藤".into(), "AC-42".into(), "राजस्व".into()],
    }
}

#[test]
fn literal_hint_shape_is_bounded_without_rewriting_names_or_injecting_a_prompt() {
    assert!(SpeechHints::default().validate().is_ok());
    assert!(hints().validate().is_ok());
    let invalid = vec![
        SpeechHints {
            languages: vec![],
            ..hints()
        },
        SpeechHints {
            languages: vec![Language::Ja, Language::Ja],
            ..hints()
        },
        SpeechHints {
            keywords: vec!["x".into(); 21],
            ..hints()
        },
        SpeechHints {
            keywords: vec!["字".repeat(81)],
            ..hints()
        },
        SpeechHints {
            keywords: (0..20).map(|n| format!("{n}{}", "字".repeat(79))).collect(),
            ..hints()
        },
    ];
    for value in invalid {
        assert!(value.validate().is_err());
    }
    for word in [
        "",
        " name",
        "name ",
        "a\nb",
        "a\rb",
        "a\tb",
        "a\0b",
        "<system>",
        "a\u{200b}b",
        "a\u{202e}b",
        "a\u{feff}",
    ] {
        assert!(
            SpeechHints {
                keywords: vec![word.into()],
                ..hints()
            }
            .validate()
            .is_err()
        );
    }
    assert!(
        serde_json::from_value::<SpeechHints>(
            json!({"languages":["ja"],"keywords":[],"prompt":"instructions"})
        )
        .is_err()
    );
    assert!(
        serde_json::from_value::<SpeechHints>(json!({"languages":["fr"],"keywords":[]})).is_err()
    );
}

#[tokio::test]
async fn http_hints_need_current_operator_scope_and_are_frozen_into_the_asr_route() {
    let backend = Arc::new(HintsBackend {
        inner: MockBackend::new(false),
        hints: Mutex::new(vec![]),
    });
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut stream = subscribe(&p, &h, Role::Operator);
    let body = json!({"generation":1,"source":"remote_mix","route":"transcription","language":"en","transcription":hints()});
    let public = viewer(&p);
    for (header, origin, generation) in [
        (Some(&public), auth::ORIGIN, 1),
        (None, auth::ORIGIN, 1),
        (Some(&h), "https://hostile.example", 1),
        (Some(&h), auth::ORIGIN, 2),
    ] {
        let mut body = body.clone();
        body["generation"] = json!(generation);
        assert_ne!(
            request(p.clone(), "POST", "/api/routes", body, header, origin)
                .await
                .0,
            StatusCode::ACCEPTED
        );
    }
    assert!(backend.hints.lock().unwrap().is_empty());
    let (status, _, _) = request(
        p.clone(),
        "POST",
        "/api/routes",
        body,
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    next_ready(&mut stream).await;
    assert!(backend.hints.lock().unwrap()[0] == hints());
    assert!(
        p.start_route_with_options(
            1,
            Source::SelfSource,
            Route::Live,
            Language::En,
            RouteOptions {
                live_mode: Some(LiveMode::Assistant),
                transcription: Some(hints()),
                interpretation: None,
            }
        )
        .is_err()
    );
    p.stop("operator_stop");
    assert!(
        p.start_route_with_options(
            p.generation(),
            Source::SelfSource,
            Route::Transcription,
            Language::En,
            RouteOptions {
                live_mode: None,
                transcription: Some(hints()),
                interpretation: None,
            }
        )
        .is_err()
    );
    assert_eq!(backend.hints.lock().unwrap().len(), 1);
}

#[tokio::test]
async fn invalid_http_hints_never_open_a_route_or_silently_fall_back() {
    let backend = Arc::new(HintsBackend {
        inner: MockBackend::new(false),
        hints: Mutex::new(vec![]),
    });
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let _stream = subscribe(&p, &h, Role::Operator);
    for value in [
        json!({"languages":[],"keywords":[]}),
        json!({"languages":["ja"],"keywords":["bad\nterm"]}),
        json!({"languages":["ja"],"keywords":[],"prompt":"not allowed"}),
    ] {
        assert_ne!(request(p.clone(),"POST","/api/routes",json!({"generation":1,"source":"remote_mix","route":"transcription","language":"en","transcription":value}),Some(&h),auth::ORIGIN).await.0,StatusCode::ACCEPTED);
    }
    assert!(backend.hints.lock().unwrap().is_empty());
    assert_eq!(backend.inner.opened.load(Ordering::SeqCst), 0);
    assert!(
        MockBackend::new(false)
            .open_transcription(Language::Ja, hints())
            .await
            .is_err()
    );
    p.stop("operator_stop");
}

#[tokio::test]
async fn live_timeline_is_private_exact_and_cannot_acquire_source_item_authority() {
    let backend = MockBackend::new(false);
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut private = subscribe(&p, &h, Role::Operator);
    let v = viewer(&p);
    let mut public = subscribe(&p, &v, Role::Public);
    p.start_route(1, Source::RemoteMix, Route::Live, Language::En)
        .unwrap();
    next_ready(&mut private).await;
    let sender = backend.events.lock().unwrap()[0].clone();
    sender
        .send(StreamEvent::LiveTranscript {
            provenance: Provenance::LiveOutput,
            text: " 12,000—not 20,000.".into(),
            start_ms: 125.25,
            end_ms: 901.75,
        })
        .await
        .unwrap();
    let value = private.events.recv().await.unwrap();
    assert_eq!(value["type"], "live_transcript");
    assert_eq!(value["start_ms"], 125.25);
    assert_eq!(value["end_ms"], 901.75);
    assert_eq!(value["text"], " 12,000—not 20,000.");
    assert!(value.get("item").is_none() && value.get("final").is_none());
    assert!(public.events.try_recv().is_err());
    sender
        .send(StreamEvent::LiveTranscript {
            provenance: Provenance::SourceAsr,
            text: "invalid source claim".into(),
            start_ms: 0.0,
            end_ms: 1.0,
        })
        .await
        .unwrap();
    assert_ne!(
        private.events.recv().await.unwrap()["type"],
        "live_transcript"
    );
    assert!(public.events.try_recv().is_err());
    p.stop("operator_stop");
}
