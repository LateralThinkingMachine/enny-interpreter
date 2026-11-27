//! Real Axum handlers and memory lifecycle, with synthetic identity/providers.
use super::*;

async fn get(
    app: axum::Router,
    host: &str,
    authorization: Option<&HeaderValue>,
) -> (StatusCode, HeaderMap, Value) {
    get_path(app, host, authorization, "/api/diagnostics").await
}
async fn get_path(
    app: axum::Router,
    host: &str,
    authorization: Option<&HeaderValue>,
    path: &str,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder().uri(path).header("host", host);
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.len() < 1024);
    (status, headers, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn concurrent_stop_posts_acknowledge_only_the_published_terminal_generation() {
    struct Gate {
        entered: tokio::sync::Notify,
        release: Mutex<std::sync::mpsc::Receiver<()>>,
    }
    impl ProviderBackend for Gate {
        fn revoke(&self) {
            self.entered.notify_one();
            self.release
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(3))
                .unwrap();
        }
        fn enabled(&self) -> bool {
            false
        }
        fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
            unreachable!("Stop cannot open a stream")
        }
        fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
            unreachable!("Stop cannot synthesize speech")
        }
    }
    // A single-thread reactor must still serve the duplicate POST and readback
    // while the winning fence is held by a separate worker.
    let (release, receiver) = std::sync::mpsc::channel();
    let gate = Arc::new(Gate {
        entered: tokio::sync::Notify::new(),
        release: Mutex::new(receiver),
    });
    let (p, ticket) = pilot(gate.clone());
    let h = operator(&p, &ticket);
    let app = crate::router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    let stop = || {
        request(
            p.clone(),
            "POST",
            "/api/stop",
            json!({}),
            Some(&h),
            auth::ORIGIN,
        )
    };
    let receipt = || {
        get_path(
            app.clone(),
            auth::HOST,
            Some(&h["authorization"]),
            "/api/stop-receipt",
        )
    };
    let mut first = Box::pin(stop());
    assert!(futures_util::poll!(&mut first).is_pending());
    tokio::time::timeout(Duration::from_secs(1), gate.entered.notified())
        .await
        .unwrap();
    assert!(p.is_stopped());
    assert_eq!(p.generation(), 1);
    let mut second = Box::pin(stop());
    assert!(futures_util::poll!(&mut second).is_pending());
    assert_eq!(receipt().await.0, StatusCode::CONFLICT);
    release.send(()).unwrap();
    let (first, second) = tokio::time::timeout(Duration::from_secs(1), async {
        tokio::join!(first, second)
    })
    .await
    .unwrap();
    for (status, _, result) in [first, second] {
        assert_eq!(status, StatusCode::OK);
        assert_eq!(result, json!({"state":"stopped","generation":2}));
    }
    let (status, _, result) = receipt().await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(result["session_generation"], 1);
    assert_eq!(result["generation"], 2);
}

#[tokio::test]
async fn operator_diagnostics_remain_readable_after_stop_without_granting_new_work() {
    let backend = MockBackend::new(false);
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let app = crate::router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    let (status, headers, before) = get(app.clone(), auth::HOST, Some(&h["authorization"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(
        before,
        json!({"version":1,"generation":1,"stopped":false,"stop":null,"last_issue":null})
    );
    p.stop("backpressure");
    p.stop("operator_stop");
    let (status, headers, stopped) = get(app.clone(), auth::HOST, Some(&h["authorization"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(stopped.as_object().unwrap().len(), 5);
    assert_eq!(stopped["version"], 1);
    assert_eq!(stopped["generation"], 2);
    assert_eq!(stopped["stopped"], true);
    assert_eq!(stopped["stop"]["code"], "backpressure");
    assert!(stopped["stop"]["elapsed_seconds"].as_u64().unwrap() <= 60);
    assert!(stopped["last_issue"].is_null());
    assert!(p.positive(2).is_err());
    assert!(
        p.subscribe(
            p.authorize(&h, Role::Operator, false).unwrap(),
            Role::Operator
        )
        .is_err()
    );
    assert_eq!(backend.opened.load(Ordering::SeqCst), 0);
    assert_eq!(
        get(app.clone(), auth::HOST, Some(&h["authorization"]))
            .await
            .2,
        stopped
    );
    p.logout(Role::Operator);
    assert_eq!(
        get(app, auth::HOST, Some(&h["authorization"])).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn service_gap_retains_only_fixed_metadata_before_routes_are_removed() {
    let backend = MockBackend::new(false);
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let mut events = subscribe(&p, &h, Role::Operator);
    let id = p
        .start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja)
        .unwrap();
    next_ready(&mut events).await;
    let sender = backend.events.lock().unwrap()[0].clone();
    sender
        .send(StreamEvent::Error(ProviderError::Protocol))
        .await
        .unwrap();
    let gap = tokio::time::timeout(Duration::from_secs(1), events.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gap["type"], "gap");
    let observed = serde_json::to_value(p.diagnostics(&h).unwrap()).unwrap();
    assert_eq!(observed["last_issue"]["code"], "provider_protocol");
    assert_eq!(observed["last_issue"]["route"], "transcription");
    assert_eq!(observed["last_issue"]["source"], "remote_mix");
    assert_eq!(observed["last_issue"].as_object().unwrap().len(), 4);
    assert!(!observed.to_string().contains(&id));
    assert!(
        !observed
            .to_string()
            .contains(h["x-pilot-csrf"].to_str().unwrap())
    );
    p.stop("operator_stop");
    p.source_gap(Source::SelfSource, "raw-secret-provider-payload");
    let after = serde_json::to_value(p.diagnostics(&h).unwrap()).unwrap();
    assert_eq!(after["last_issue"], observed["last_issue"]);
    assert_eq!(after["stop"]["code"], "operator_stop");
    assert!(!after.to_string().contains("raw-secret"));
}

#[tokio::test]
async fn numeric_asr_error_reaches_only_the_authorized_read_and_survives_stop() {
    let backend = MockBackend::new(false);
    let (p, ticket) = pilot(backend.clone());
    let h = operator(&p, &ticket);
    let public = viewer(&p);
    let mut events = subscribe(&p, &h, Role::Operator);
    let id = p
        .start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja)
        .unwrap();
    next_ready(&mut events).await;
    let sender = backend.events.lock().unwrap()[0].clone();
    sender
        .send(StreamEvent::Error(ProviderError::AsrUsageMismatch {
            submitted_ms: 1_100,
            reported_ms: Some(2_101),
            allowed_ms: 2_100,
        }))
        .await
        .unwrap();
    let gap = tokio::time::timeout(Duration::from_secs(1), events.events.recv())
        .await
        .unwrap()
        .unwrap();
    assert_eq!(gap["code"], "usage_mismatch");
    assert!(gap.get("asr_usage").is_none());
    let app = crate::router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    let public_app = crate::public_router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    let (status, headers, observed) = get(app.clone(), auth::HOST, Some(&h["authorization"])).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(
        observed["last_issue"]["asr_usage"],
        json!({"submitted_ms":1100,"reported_ms":2101,"allowed_ms":2100})
    );
    assert_eq!(observed["last_issue"].as_object().unwrap().len(), 5);
    assert!(!observed.to_string().contains(&id));
    assert!(
        !observed
            .to_string()
            .contains(h["x-pilot-csrf"].to_str().unwrap())
    );
    for (router, host, authorization, expected) in [
        (app.clone(), auth::HOST, None, StatusCode::UNAUTHORIZED),
        (
            app.clone(),
            auth::HOST,
            Some(&public["authorization"]),
            StatusCode::UNAUTHORIZED,
        ),
        (
            public_app,
            auth::PUBLIC_HOST,
            Some(&h["authorization"]),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, _, body) = get(router, host, authorization).await;
        assert_eq!(status, expected);
        assert!(body.get("last_issue").is_none());
        assert!(!body.to_string().contains("reported_ms"));
    }
    p.stop("operator_stop");
    p.source_gap(Source::SelfSource, "raw-private-provider-payload");
    let after = get(app.clone(), auth::HOST, Some(&h["authorization"]))
        .await
        .2;
    assert_eq!(after["last_issue"], observed["last_issue"]);
    assert!(!after.to_string().contains("raw-private"));
    assert_eq!(
        get(app.clone(), auth::HOST, Some(&h["authorization"]))
            .await
            .2,
        after
    );
    p.logout(Role::Operator);
    assert_eq!(
        get(app, auth::HOST, Some(&h["authorization"])).await.0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn latest_pre_stop_source_issue_has_no_invented_route_and_sanitizes_unknown_codes() {
    let (p, ticket) = pilot(engine::disabled());
    let h = operator(&p, &ticket);
    let _events = subscribe(&p, &h, Role::Operator);
    p.source_gap(Source::RemoteMix, "sequence_gap");
    p.source_gap(Source::SelfSource, "untrusted-provider-data");
    p.stop("untrusted-stop-data");
    let value = serde_json::to_value(p.diagnostics(&h).unwrap()).unwrap();
    assert_eq!(value["last_issue"]["code"], "unknown");
    assert_eq!(value["last_issue"]["source"], "self");
    assert!(value["last_issue"]["route"].is_null());
    assert_eq!(value["stop"]["code"], "unknown");
    assert!(!value.to_string().contains("untrusted"));
}

#[tokio::test]
async fn diagnostic_read_denies_missing_foreign_public_and_cross_origin_authority() {
    let (p, ticket) = pilot(engine::disabled());
    let h = operator(&p, &ticket);
    let public = viewer(&p);
    let app = crate::router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    let public_app = crate::public_router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    for (router, host, authorization, expected) in [
        (app.clone(), auth::HOST, None, StatusCode::UNAUTHORIZED),
        (
            app.clone(),
            auth::HOST,
            Some(&public["authorization"]),
            StatusCode::UNAUTHORIZED,
        ),
        (
            public_app.clone(),
            auth::PUBLIC_HOST,
            Some(&h["authorization"]),
            StatusCode::FORBIDDEN,
        ),
        (
            public_app,
            auth::PUBLIC_HOST,
            Some(&public["authorization"]),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (status, headers, value) = get(router, host, authorization).await;
        assert_eq!(status, expected);
        assert_eq!(headers["cache-control"], "no-store");
        assert!(value.get("last_issue").is_none());
    }
    for (method, path, body, origin, expected) in [
        (
            "GET",
            "/api/diagnostics",
            "",
            "https://untrusted.example",
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/diagnostics",
            "",
            auth::PUBLIC_ORIGIN,
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/diagnostics?include=text",
            "",
            auth::ORIGIN,
            StatusCode::FORBIDDEN,
        ),
        (
            "GET",
            "/api/diagnostics",
            "private-input",
            auth::ORIGIN,
            StatusCode::BAD_REQUEST,
        ),
        (
            "POST",
            "/api/diagnostics",
            "{}",
            auth::ORIGIN,
            StatusCode::METHOD_NOT_ALLOWED,
        ),
    ] {
        let response = app
            .clone()
            .oneshot(
                Request::builder()
                    .method(method)
                    .uri(path)
                    .header("host", auth::HOST)
                    .header("origin", origin)
                    .header("content-type", "application/json")
                    .header("authorization", &h["authorization"])
                    .body(Body::from(body))
                    .unwrap(),
            )
            .await
            .unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        let value: Value = serde_json::from_slice(&bytes).unwrap();
        assert!(value.get("last_issue").is_none());
        assert!(!String::from_utf8_lossy(&bytes).contains("private-input"));
    }
    p.stop("operator_stop");
}

#[tokio::test]
async fn stopped_diagnostic_read_does_not_extend_the_original_identity_deadline() {
    let (p, ticket) = Pilot::new(engine::disabled(), Duration::from_millis(30), 1).unwrap();
    let h = operator(&p, &ticket);
    p.stop("operator_stop");
    tokio::time::sleep(Duration::from_millis(35)).await;
    let app = crate::router(p, PathBuf::from("/private/tmp/no-assets"));
    let (status, headers, value) = get(app, auth::HOST, Some(&h["authorization"])).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(headers["cache-control"], "no-store");
    assert!(value.get("stop").is_none() && value.get("last_issue").is_none());
}
