use crate::{
    auth::{self, ApiError, Role},
    engine::{
        self, Language, PreparedAudio, ProviderBackend, ProviderError, ProviderFuture, Route,
        StreamEvent, StreamHandle,
    },
    runtime::{Pilot, Source},
};
#[path = "budget_tests.rs"]
mod budget_tests;
#[path = "diagnostics_tests.rs"]
mod diagnostics_tests;
#[path = "interpretation_tests.rs"]
mod interpretation_tests;
#[path = "origins_tests.rs"]
mod origins_tests;
#[path = "speech_options_tests.rs"]
mod speech_options_tests;
use axum::{
    body::Body,
    http::{HeaderMap, HeaderValue, Request, StatusCode},
};
use http_body_util::BodyExt;
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicUsize, Ordering},
    },
    time::Duration,
};
use tokio::sync::{Notify, mpsc, watch};
use tower::ServiceExt;

struct MockBackend {
    opened: AtomicUsize,
    events: Mutex<Vec<mpsc::Sender<StreamEvent>>>,
    tts_wait: bool,
    tts_started: Arc<Notify>,
    hold_ready: AtomicBool,
    counts: Mutex<Vec<Arc<AtomicUsize>>>,
    releases: Mutex<Vec<Arc<Notify>>>,
}
impl MockBackend {
    fn new(tts_wait: bool) -> Arc<Self> {
        Arc::new(Self {
            opened: AtomicUsize::new(0),
            events: Mutex::new(Vec::new()),
            tts_wait,
            tts_started: Arc::new(Notify::new()),
            hold_ready: AtomicBool::new(false),
            counts: Mutex::new(Vec::new()),
            releases: Mutex::new(Vec::new()),
        })
    }
}
impl ProviderBackend for MockBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        let (input, mut input_rx) = mpsc::channel(8);
        let (events, receiver) = mpsc::channel(32);
        let (stop, mut stop_rx) = watch::channel(false);
        let held = self.hold_ready.load(Ordering::SeqCst);
        let release = Arc::new(Notify::new());
        let count = Arc::new(AtomicUsize::new(0));
        self.counts.lock().unwrap().push(count.clone());
        self.releases.lock().unwrap().push(release.clone());
        self.events.lock().unwrap().push(events.clone());
        Box::pin(async move {
            if !held {
                let _ = events
                    .send(StreamEvent::Ready {
                        expires_in_seconds: 50,
                    })
                    .await;
            }
            tokio::spawn(async move {
                if held {
                    tokio::select! {biased;_=stop_rx.changed()=>return,_=release.notified()=>{}}
                }
                loop {
                    tokio::select! {biased;_=stop_rx.changed()=>break,event=input_rx.recv()=>if event.is_none(){break}else{count.fetch_add(1,Ordering::SeqCst);}}
                }
            });
            Ok(StreamHandle {
                renewal_fence: None,
                input,
                events: receiver,
                stop,
            })
        })
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        let wait = self.tts_wait;
        let started = self.tts_started.clone();
        Box::pin(async move {
            started.notify_one();
            if wait {
                std::future::pending::<()>().await;
            }
            Ok(wav())
        })
    }
}
async fn next_ready(subscription: &mut crate::runtime::Subscription) -> Value {
    let starting = subscription.events.recv().await.unwrap();
    assert_eq!(starting["state"], "starting");
    let ready = subscription.events.recv().await.unwrap();
    assert_eq!(ready["state"], "ready");
    ready
}
fn wav() -> PreparedAudio {
    let mut bytes = Vec::new();
    let n = 24000u32;
    bytes.extend(b"RIFF");
    bytes.extend((36 + n).to_le_bytes());
    bytes.extend(b"WAVEfmt ");
    bytes.extend(16u32.to_le_bytes());
    bytes.extend([1, 0, 1, 0]);
    bytes.extend(24000u32.to_le_bytes());
    bytes.extend(48000u32.to_le_bytes());
    bytes.extend([2, 0, 16, 0]);
    bytes.extend(b"data");
    bytes.extend(n.to_le_bytes());
    bytes.resize(44 + n as usize, 0);
    PreparedAudio {
        wav: bytes,
        duration_ms: 500,
        sample_rate: 24000,
    }
}
fn pilot(backend: Arc<dyn ProviderBackend>) -> (Arc<Pilot>, String) {
    Pilot::new(backend, Duration::from_secs(60), 30).unwrap()
}
fn headers(token: &str, csrf: &str) -> HeaderMap {
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    headers.insert("x-pilot-csrf", HeaderValue::from_str(csrf).unwrap());
    headers
}
fn operator(pilot: &Pilot, ticket: &str) -> HeaderMap {
    let (view, token) = pilot.redeem(ticket, Role::Operator).unwrap();
    headers(&token, view["csrf"].as_str().unwrap())
}
fn viewer(pilot: &Pilot) -> HeaderMap {
    let t = pilot.public_ticket(1).unwrap();
    let (view, token) = pilot
        .redeem(t["ticket"].as_str().unwrap(), Role::Public)
        .unwrap();
    headers(&token, view["csrf"].as_str().unwrap())
}
fn subscribe(pilot: &Pilot, h: &HeaderMap, role: Role) -> crate::runtime::Subscription {
    let session = pilot.authorize(h, role, false).unwrap();
    pilot.subscribe(session, role).unwrap()
}

#[test]
fn capabilities_require_explicit_provider_and_roles_do_not_cross() {
    let (p, t) = pilot(engine::disabled());
    let h = operator(&p, &t);
    let session = p.session(&h, Role::Operator).unwrap();
    assert_eq!(session["providers_enabled"], false);
    assert_eq!(session["capabilities"]["source_captions"], false);
    let public = viewer(&p);
    assert!(matches!(
        p.authorize(&public, Role::Operator, false),
        Err(ApiError::Unauthorized)
    ));
    assert!(matches!(
        p.authorize(&h, Role::Public, false),
        Err(ApiError::Unauthorized)
    ));
    assert_eq!(
        p.session(&public, Role::Public).unwrap()["capabilities"]["source_captions"],
        false
    );
}

#[test]
fn one_use_ticket_csrf_duplicate_bearer_and_viewer_rotation_are_enforced() {
    let (p, t) = pilot(engine::disabled());
    let mut h = operator(&p, &t);
    assert!(p.redeem(&t, Role::Operator).is_err());
    h.remove("x-pilot-csrf");
    assert!(matches!(
        p.authorize(&h, Role::Operator, true),
        Err(ApiError::Forbidden)
    ));
    let existing = h["authorization"].clone();
    h.append("authorization", existing);
    assert!(p.authorize(&h, Role::Operator, false).is_err());
    let old = viewer(&p);
    let _new = p.public_ticket(1).unwrap();
    assert!(p.authorize(&old, Role::Public, false).is_err());
}

#[test]
fn stop_is_sticky_idempotent_and_cannot_be_cleared_by_new_socket() {
    let (p, t) = pilot(engine::disabled());
    let h = operator(&p, &t);
    let id = p.authorize(&h, Role::Operator, false).unwrap();
    p.stop("operator_stop");
    p.stop("operator_stop");
    assert_eq!(p.generation(), 2);
    assert!(p.is_stopped());
    assert!(p.subscribe(id, Role::Operator).is_err());
    assert!(p.public_ticket(2).is_err());
    assert_eq!(p.session(&h, Role::Operator).unwrap()["stopped"], true);
}

#[tokio::test]
async fn prepared_public_clip_requires_audition_exact_digest_viewer_and_one_attempt() {
    let (p, t) = pilot(MockBackend::new(false));
    let h = operator(&p, &t);
    let _operator = subscribe(&p, &h, Role::Operator);
    let clip = p
        .prepare(1, "Exact approved words 42".into(), Language::En)
        .await
        .unwrap();
    assert!(p.approve(1, &clip.id, &clip.digest).is_err());
    assert!(p.audition(1, &clip.id, "wrong").is_err());
    p.audition(1, &clip.id, &clip.digest).unwrap();
    assert!(p.approve(1, &clip.id, &clip.digest).is_err());
    let public = viewer(&p);
    let mut public_stream = subscribe(&p, &public, Role::Public);
    let delivery = p.approve(1, &clip.id, &clip.digest).unwrap();
    let event = public_stream.events.recv().await.unwrap();
    assert_eq!(event["text"], "Exact approved words 42");
    assert_eq!(event["digest"], clip.digest);
    assert_eq!(event["type"], "delivery");
    assert!(!event.to_string().contains("csrf"));
    assert!(p.approve(1, &clip.id, &clip.digest).is_err());
    let public_id = p.authorize(&public, Role::Public, false).unwrap();
    let bytes = p.public_audio(&public_id, &delivery).unwrap();
    assert_eq!(bytes.as_ref(), &wav().wav);
    assert!(p.public_audio(&public_id, &delivery).is_err());
    p.receipt(&public_id, 1, &delivery, "accepted").unwrap();
    p.receipt(&public_id, 1, &delivery, "playback_started")
        .unwrap();
    p.receipt(&public_id, 1, &delivery, "playback_ended")
        .unwrap();
    assert!(p.approve(1, &clip.id, &clip.digest).is_err());
}

#[tokio::test]
async fn delayed_preparation_after_stop_cannot_install_a_clip() {
    let backend = MockBackend::new(true);
    let (p, t) = pilot(backend.clone());
    let _h = operator(&p, &t);
    let future = {
        let p = p.clone();
        tokio::spawn(async move { p.prepare(1, "delayed".into(), Language::Ja).await })
    };
    backend.tts_started.notified().await;
    p.stop("operator_stop");
    assert!(matches!(
        tokio::time::timeout(Duration::from_millis(200), future)
            .await
            .unwrap()
            .unwrap(),
        Err(ApiError::Stopped)
    ));
    assert!(p.preview_audio("anything").is_err());
}

#[tokio::test]
async fn late_route_start_is_not_polled_after_stop() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let _stream = subscribe(&p, &h, Role::Operator);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
        .unwrap();
    p.stop("operator_stop");
    tokio::task::yield_now().await;
    assert_eq!(backend.opened.load(Ordering::SeqCst), 0);
    // Even the provider future constructor was not called after the Stop.
    assert!(
        p.start_route(2, Source::RemoteMix, Route::Transcription, Language::En)
            .is_err()
    );
    assert!(p.is_stopped());
}

struct StartupFailureBackend {
    error: Option<ProviderError>,
    opened: AtomicUsize,
    polled: Arc<Notify>,
    dropped: Arc<AtomicBool>,
}
struct StartupFutureDropped(Arc<AtomicBool>);
impl Drop for StartupFutureDropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}
impl StartupFailureBackend {
    fn new(error: Option<ProviderError>) -> Arc<Self> {
        Arc::new(Self {
            error,
            opened: AtomicUsize::new(0),
            polled: Arc::new(Notify::new()),
            dropped: Arc::new(AtomicBool::new(false)),
        })
    }
}
impl ProviderBackend for StartupFailureBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        self.opened.fetch_add(1, Ordering::SeqCst);
        let error = self.error;
        let polled = self.polled.clone();
        let dropped = self.dropped.clone();
        Box::pin(async move {
            let _drop = StartupFutureDropped(dropped);
            polled.notify_one();
            match error {
                Some(error) => Err(error),
                None => std::future::pending().await,
            }
        })
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
}

#[tokio::test]
async fn runtime_startup_preserves_safe_failure_categories_and_closes_the_source() {
    for (error, expected) in [
        (ProviderError::Budget, "budget_exhausted"),
        (ProviderError::Protocol, "provider_protocol"),
        (ProviderError::Deadline, "deadline"),
        (ProviderError::Unavailable, "provider_unavailable"),
        (ProviderError::Disabled, "provider_unavailable"),
    ] {
        let backend = StartupFailureBackend::new(Some(error));
        let (p, t) = pilot(backend.clone());
        let h = operator(&p, &t);
        let mut private = subscribe(&p, &h, Role::Operator);
        let public = viewer(&p);
        let mut public_stream = subscribe(&p, &public, Role::Public);
        let id = p
            .start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja)
            .unwrap();
        let starting = private.events.recv().await.unwrap();
        assert_eq!(starting["route_id"], id);
        assert_eq!(starting["state"], "starting");
        let failure = tokio::time::timeout(Duration::from_secs(1), private.events.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(
            failure,
            json!({"type":"gap","generation":1,"source":"remote_mix","code":expected}),
            "startup error: {error:?}"
        );
        assert!(public_stream.events.try_recv().is_err());
        assert!(matches!(
            p.start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja),
            Err(ApiError::Stopped)
        ));
        assert_eq!(backend.opened.load(Ordering::SeqCst), 1);
        assert!(backend.dropped.load(Ordering::SeqCst));
        assert!(!p.is_stopped());
        p.stop("operator_stop");
    }
}

#[tokio::test(start_paused = true)]
async fn runtime_startup_timeout_reports_deadline_and_drops_the_pending_future() {
    let backend = StartupFailureBackend::new(None);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja)
        .unwrap();
    assert_eq!(private.events.recv().await.unwrap()["state"], "starting");
    backend.polled.notified().await;
    tokio::time::advance(Duration::from_secs(11)).await;
    let failure = private.events.recv().await.unwrap();
    assert_eq!(
        failure,
        json!({"type":"gap","generation":1,"source":"remote_mix","code":"deadline"})
    );
    assert!(backend.dropped.load(Ordering::SeqCst));
    assert_eq!(backend.opened.load(Ordering::SeqCst), 1);
    assert!(!p.is_stopped());
    p.stop("operator_stop");
}

#[tokio::test]
async fn runtime_startup_stop_cancels_without_a_spurious_failure_category() {
    let backend = StartupFailureBackend::new(None);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::Ja)
        .unwrap();
    assert_eq!(private.events.recv().await.unwrap()["state"], "starting");
    backend.polled.notified().await;
    p.stop("operator_stop");
    assert_eq!(private.events.recv().await.unwrap()["type"], "stopped");
    tokio::task::yield_now().await;
    assert!(backend.dropped.load(Ordering::SeqCst));
    assert!(private.events.try_recv().is_err());
    assert_eq!(backend.opened.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn confirmed_normal_expiry_allows_only_explicit_route_renewal() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    let first = p
        .start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
        .unwrap();
    let ready = next_ready(&mut private).await;
    assert_eq!(ready["expires_in_seconds"], 50);
    backend.events.lock().unwrap()[0]
        .try_send(StreamEvent::Closed { confirmed: true })
        .unwrap();
    let closed = private.events.recv().await.unwrap();
    assert_eq!(closed["state"], "closed");
    assert!(closed["expires_in_seconds"].is_null());
    assert_eq!(backend.opened.load(Ordering::SeqCst), 1);
    assert!(!p.is_stopped());
    let second = p
        .start_route(1, Source::RemoteMix, Route::Transcription, Language::Hi)
        .unwrap();
    assert_ne!(first, second);
    next_ready(&mut private).await;
    p.stop("operator_stop");
}

#[tokio::test(start_paused = true)]
async fn rejected_out_of_order_receipt_does_not_suppress_unknown_outcome() {
    let (p, t) = pilot(MockBackend::new(false));
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    let public = viewer(&p);
    let stream = subscribe(&p, &public, Role::Public);
    let clip = p
        .prepare(1, "No false acknowledgement".into(), Language::En)
        .await
        .unwrap();
    p.audition(1, &clip.id, &clip.digest).unwrap();
    let id = p.approve(1, &clip.id, &clip.digest).unwrap();
    p.public_audio(&stream.session, &id).unwrap();
    assert!(matches!(
        p.receipt(&stream.session, 1, &id, "playback_ended"),
        Err(ApiError::Conflict)
    ));
    tokio::task::yield_now().await;
    tokio::time::advance(Duration::from_secs(4)).await;
    tokio::task::yield_now().await;
    let event = private.events.recv().await.unwrap();
    assert_eq!(event["state"], "unknown");
}

#[tokio::test]
async fn public_surface_rejects_operator_requests_even_with_operator_authorization() {
    let (p, t) = pilot(engine::disabled());
    let operator_headers = operator(&p, &t);
    let public_headers = viewer(&p);
    let public = crate::public_router(p.clone(), PathBuf::from("/private/tmp/no-pilot-assets"));
    for path in [
        "/api/session",
        "/api/previews/private/audio",
        "/api/bootstrap",
        "/api/operator/events",
    ] {
        let request = Request::builder()
            .uri(path)
            .header("host", auth::PUBLIC_HOST)
            .header("origin", auth::PUBLIC_ORIGIN)
            .header("authorization", &operator_headers["authorization"])
            .body(Body::empty())
            .unwrap();
        let response = public.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(
            !String::from_utf8_lossy(&bytes)
                .contains(operator_headers["x-pilot-csrf"].to_str().unwrap())
        );
    }
    let request = Request::builder()
        .uri("/api/public/session")
        .header("host", auth::PUBLIC_HOST)
        .header("origin", auth::PUBLIC_ORIGIN)
        .header("authorization", &public_headers["authorization"])
        .body(Body::empty())
        .unwrap();
    let response = public.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()["cross-origin-opener-policy"],
        "unsafe-none"
    );
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["role"], "public");
    assert_eq!(
        body["csrf"],
        public_headers["x-pilot-csrf"].to_str().unwrap()
    );
    let request = Request::builder()
        .uri("/api/session")
        .header("host", auth::HOST)
        .header("origin", auth::PUBLIC_ORIGIN)
        .header("authorization", &operator_headers["authorization"])
        .body(Body::empty())
        .unwrap();
    let response = crate::router(p, PathBuf::from("/private/tmp/no-pilot-assets"))
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn malformed_json_errors_never_echo_untrusted_content() {
    let (p, t) = pilot(engine::disabled());
    let h = operator(&p, &t);
    let (status, _, body) = request(
        p,
        "POST",
        "/api/routes",
        json!({"generation":1,"source":"PRIVATE_SECRET_CANARY","route":"live","language":"en"}),
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    assert_eq!(body["error"], "invalid_request");
    assert!(!body.to_string().contains("PRIVATE_SECRET_CANARY"));
}

#[tokio::test]
async fn active_capture_never_fills_a_second_route_before_its_ready_ack() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
        .unwrap();
    next_ready(&mut private).await;
    backend.hold_ready.store(true, Ordering::SeqCst);
    p.start_route(1, Source::RemoteMix, Route::Live, Language::En)
        .unwrap();
    assert_eq!(private.events.recv().await.unwrap()["state"], "starting");
    tokio::task::yield_now().await;
    let mut frame = vec![0; 646];
    frame[0] = 1;
    for seq in 0..20u32 {
        frame[2..6].copy_from_slice(&seq.to_le_bytes());
        p.frame(&frame).unwrap();
        tokio::time::sleep(Duration::from_millis(21)).await;
    }
    assert_eq!(backend.counts.lock().unwrap()[0].load(Ordering::SeqCst), 20);
    assert_eq!(backend.counts.lock().unwrap()[1].load(Ordering::SeqCst), 0);
    assert!(!p.is_stopped());
    backend.releases.lock().unwrap()[1].notify_one();
    backend.events.lock().unwrap()[1]
        .try_send(StreamEvent::Ready {
            expires_in_seconds: 50,
        })
        .unwrap();
    assert_eq!(private.events.recv().await.unwrap()["state"], "ready");
    frame[2..6].copy_from_slice(&20u32.to_le_bytes());
    p.frame(&frame).unwrap();
    tokio::task::yield_now().await;
    assert_eq!(backend.counts.lock().unwrap()[1].load(Ordering::SeqCst), 1);
    p.stop("operator_stop");
}

#[tokio::test]
async fn unicode_text_byte_limits_reject_before_provider_work() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    let route = p
        .start_route(1, Source::RemoteMix, Route::Live, Language::Ja)
        .unwrap();
    next_ready(&mut private).await;
    assert!(matches!(
        p.live_text(1, &route, "あ".repeat(134)),
        Err(ApiError::Invalid)
    ));
    assert_eq!(backend.counts.lock().unwrap()[0].load(Ordering::SeqCst), 0);
    assert!(matches!(
        p.prepare(1, "あ".repeat(667), Language::Ja).await,
        Err(ApiError::Invalid)
    ));
    assert!(!p.is_stopped());
    p.stop("operator_stop");
}

#[tokio::test]
async fn stop_revokes_remaining_approved_audio_http_chunks() {
    let (p, t) = pilot(MockBackend::new(false));
    let h = operator(&p, &t);
    let clip = p
        .prepare(1, "Checked between chunks".into(), Language::En)
        .await
        .unwrap();
    let mut request = Request::builder()
        .uri(&clip.audio_path)
        .header("host", auth::HOST);
    for (name, value) in &h {
        request = request.header(name, value);
    }
    let response = crate::router(p.clone(), PathBuf::from("/private/tmp/no-pilot-assets"))
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    let mut body = response.into_body();
    let first = body.frame().await.unwrap().unwrap().into_data().unwrap();
    assert_eq!(first.len(), 16 * 1024);
    p.stop("operator_stop");
    assert!(body.frame().await.unwrap().is_err());
}

#[test]
fn private_config_rejects_readable_permissions_symlink_oversize_and_unknown_fields() {
    use std::{
        fs,
        os::unix::fs::{PermissionsExt, symlink},
    };
    let directory = PathBuf::from(format!(
        "/private/tmp/pilot-config-test-{}",
        auth::secret().unwrap()
    ));
    fs::create_dir(&directory).unwrap();
    fs::set_permissions(&directory, fs::Permissions::from_mode(0o700)).unwrap();
    let path = directory.join("config.json");
    let valid = json!({"assets_dir":directory,"credentials_path":directory.join("credentials.json"),"duration_seconds":60,"max_input_audio_seconds":60});
    fs::write(&path, valid.to_string()).unwrap();
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    assert!(crate::config::Config::load(&path).is_ok());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
    assert!(crate::config::Config::load(&path).is_err());
    fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
    let link = directory.join("link.json");
    symlink(&path, &link).unwrap();
    assert!(crate::config::Config::load(&link).is_err());
    let mut extra = valid;
    extra["provider_url"] = json!("https://unregistered.invalid");
    fs::write(&path, extra.to_string()).unwrap();
    assert!(crate::config::Config::load(&path).is_err());
    fs::write(&path, vec![b'x'; 16 * 1024 + 1]).unwrap();
    assert!(crate::config::Config::load(&path).is_err());
    assert_eq!(fs::read_dir(&directory).unwrap().count(), 2);
    fs::remove_dir_all(directory).unwrap();
}

#[tokio::test]
async fn private_provider_text_does_not_enter_public_stream_and_overflow_stops() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    let public = viewer(&p);
    let mut public_stream = subscribe(&p, &public, Role::Public);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
        .unwrap();
    let ready = next_ready(&mut private).await;
    assert_eq!(ready["state"], "ready");
    let sender = backend.events.lock().unwrap()[0].clone();
    sender
        .send(StreamEvent::Transcript {
            provenance: engine::Provenance::SourceAsr,
            text: "PRIVATE_CANARY_42".into(),
            final_fragment: true,
            item: Some(engine::TranscriptItem {
                source_audio: None,
                id: "item_1".into(),
                revision: 1,
                sequence: 1,
            }),
        })
        .await
        .unwrap();
    assert_eq!(
        private.events.recv().await.unwrap()["text"],
        "PRIVATE_CANARY_42"
    );
    assert!(public_stream.events.try_recv().is_err());
    for i in 2..42 {
        if sender
            .send(StreamEvent::Transcript {
                provenance: engine::Provenance::SourceAsr,
                text: "bounded".into(),
                final_fragment: true,
                item: Some(engine::TranscriptItem {
                    source_audio: None,
                    id: format!("item_{i}"),
                    revision: 1,
                    sequence: i,
                }),
            })
            .await
            .is_err()
        {
            break;
        }
    }
    tokio::task::yield_now().await;
    assert!(p.is_stopped());
}

#[tokio::test]
async fn sequence_gap_closes_only_affected_source_and_budget_is_bounded() {
    let (p, t) = pilot(MockBackend::new(false));
    let h = operator(&p, &t);
    let _stream = subscribe(&p, &h, Role::Operator);
    let mut frame = vec![0u8; 646];
    frame[0] = 1;
    p.frame(&frame).unwrap();
    assert!(p.frame(&frame).is_err());
    assert!(!p.is_stopped());
    frame[1] = 1;
    p.frame(&frame).unwrap();
    assert!(
        p.start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
            .is_err()
    );
    for sequence in 1..10u32 {
        frame[2..6].copy_from_slice(&sequence.to_le_bytes());
        p.frame(&frame).unwrap()
    }
    frame[2..6].copy_from_slice(&10u32.to_le_bytes());
    assert!(p.frame(&frame).is_err());
}

#[tokio::test]
async fn wrong_provenance_is_not_projected_as_source_asr() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
        .unwrap();
    next_ready(&mut private).await;
    backend.events.lock().unwrap()[0]
        .try_send(StreamEvent::Transcript {
            provenance: engine::Provenance::LiveOutput,
            text: "not source".into(),
            final_fragment: true,
            item: None,
        })
        .unwrap();
    let event = private.events.recv().await.unwrap();
    assert_eq!(event["type"], "gap");
    assert_eq!(event["code"], "provider_protocol");
    assert!(!event.to_string().contains("not source"));
}

#[tokio::test]
async fn viewer_disconnect_invalidates_delivery_without_replay() {
    let (p, t) = pilot(MockBackend::new(false));
    let h = operator(&p, &t);
    let _private = subscribe(&p, &h, Role::Operator);
    let public = viewer(&p);
    let stream = subscribe(&p, &public, Role::Public);
    let clip = p
        .prepare(1, "Once only".into(), Language::Hi)
        .await
        .unwrap();
    p.audition(1, &clip.id, &clip.digest).unwrap();
    let delivery = p.approve(1, &clip.id, &clip.digest).unwrap();
    p.disconnected(&stream.id, Role::Public);
    assert!(p.public_audio(&stream.session, &delivery).is_err());
    let mut next = subscribe(&p, &public, Role::Public);
    assert!(next.events.try_recv().is_err());
    assert!(p.approve(1, &clip.id, &clip.digest).is_err());
}

async fn request(
    pilot: Arc<Pilot>,
    method: &str,
    path: &str,
    body: Value,
    h: Option<&HeaderMap>,
    origin: &str,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", auth::HOST)
        .header("origin", origin)
        .header("content-type", "application/json");
    if let Some(h) = h {
        for (name, value) in h {
            request = request.header(name, value);
        }
    }
    let response = crate::router(pilot, PathBuf::from("/private/tmp/no-pilot-assets"))
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    let value = serde_json::from_slice(&bytes).unwrap_or(Value::Null);
    (status, headers, value)
}

#[tokio::test]
async fn finish_http_requires_exact_operator_scope_csrf_body_and_single_generation() {
    let (p, t) = pilot(engine::disabled());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    let public = viewer(&p);
    for (body, identity, origin, expected) in [
        (
            json!({"generation":1}),
            None,
            auth::ORIGIN,
            StatusCode::UNAUTHORIZED,
        ),
        (
            json!({"generation":1}),
            Some(&public),
            auth::ORIGIN,
            StatusCode::UNAUTHORIZED,
        ),
        (
            json!({"generation":1}),
            Some(&h),
            auth::PUBLIC_ORIGIN,
            StatusCode::FORBIDDEN,
        ),
        (
            json!({"generation":2}),
            Some(&h),
            auth::ORIGIN,
            StatusCode::CONFLICT,
        ),
        (
            json!({"generation":1,"force":true}),
            Some(&h),
            auth::ORIGIN,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"generation":null}),
            Some(&h),
            auth::ORIGIN,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
        (
            json!({"generation":true}),
            Some(&h),
            auth::ORIGIN,
            StatusCode::UNPROCESSABLE_ENTITY,
        ),
    ] {
        let (status, _, _) =
            request(p.clone(), "POST", "/api/finish", body, identity, origin).await;
        assert_eq!(status, expected);
        assert!(p.positive(1).is_ok());
    }
    let mut missing = h.clone();
    missing.remove("x-pilot-csrf");
    assert_eq!(
        request(
            p.clone(),
            "POST",
            "/api/finish",
            json!({"generation":1}),
            Some(&missing),
            auth::ORIGIN
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    let (status, headers, value) = request(
        p.clone(),
        "POST",
        "/api/finish",
        json!({"generation":1}),
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(value, json!({"state":"finishing","generation":1}));
    assert_eq!(
        private.events.recv().await.unwrap(),
        json!({"type":"finishing","generation":1})
    );
    assert_eq!(
        private.events.recv().await.unwrap(),
        json!({"type":"finished","generation":1,"source_completion":"no_active_asr"})
    );
    assert_eq!(
        request(
            p.clone(),
            "POST",
            "/api/finish",
            json!({"generation":1}),
            Some(&h),
            auth::ORIGIN
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert!(!p.is_stopped());
    assert_eq!(
        request(
            p.clone(),
            "POST",
            "/api/stop",
            json!({}),
            Some(&h),
            auth::ORIGIN
        )
        .await
        .0,
        StatusCode::OK
    );
    assert!(p.is_stopped());
}

#[tokio::test]
async fn public_listener_rejects_finish_even_with_operator_authorization() {
    let (p, t) = pilot(engine::disabled());
    let operator = operator(&p, &t);
    let request = Request::builder()
        .method("POST")
        .uri("/api/finish")
        .header("host", auth::PUBLIC_HOST)
        .header("origin", auth::PUBLIC_ORIGIN)
        .header("authorization", &operator["authorization"])
        .header("x-pilot-csrf", operator["x-pilot-csrf"].clone())
        .header("content-type", "application/json")
        .body(Body::from(r#"{"generation":1}"#))
        .unwrap();
    let response = crate::public_router(p.clone(), PathBuf::from("/private/tmp/no-pilot-assets"))
        .oneshot(request)
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert!(p.positive(1).is_ok());
}

#[tokio::test]
async fn actual_router_rejects_origin_csrf_overposting_disabled_routes_and_public_role() {
    let (p, t) = pilot(engine::disabled());
    let (status, _, _) = request(
        p.clone(),
        "POST",
        "/api/bootstrap",
        json!({"ticket":t}),
        None,
        "https://hostile.example",
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, response_headers, view) = request(
        p.clone(),
        "POST",
        "/api/bootstrap",
        json!({"ticket":t}),
        None,
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert!(!response_headers.contains_key("set-cookie"));
    assert_eq!(response_headers["cache-control"], "no-store");
    let token = response_headers["x-pilot-session"].to_str().unwrap();
    let h = headers(token, view["csrf"].as_str().unwrap());
    let (status, _, _) = request(
        p.clone(),
        "POST",
        "/api/routes",
        json!({"generation":1,"source":"remote_mix","route":"live","language":"en"}),
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    let(status,_,_)=request(p.clone(),"POST","/api/routes",json!({"generation":1,"source":"remote_mix","route":"live","language":"en","role":"operator"}),Some(&h),auth::ORIGIN).await;
    assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
    let public = viewer(&p);
    let (status, _, _) = request(
        p.clone(),
        "POST",
        "/api/stop",
        json!({}),
        Some(&public),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let mut no_csrf = h.clone();
    no_csrf.remove("x-pilot-csrf");
    let (status, _, _) = request(
        p.clone(),
        "POST",
        "/api/stop",
        json!({}),
        Some(&no_csrf),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let (status, _, view) = request(
        p.clone(),
        "POST",
        "/api/stop",
        json!({}),
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(view["generation"], 2);
    let (status, _, _) = request(
        p,
        "POST",
        "/api/public/tickets",
        json!({"generation":2}),
        Some(&h),
        auth::ORIGIN,
    )
    .await;
    assert_eq!(status, StatusCode::CONFLICT);
}

#[tokio::test]
async fn source_item_revisions_tombstone_and_final_seal_remain_private() {
    let backend = MockBackend::new(false);
    let (p, t) = pilot(backend.clone());
    let h = operator(&p, &t);
    let mut private = subscribe(&p, &h, Role::Operator);
    let public = viewer(&p);
    let mut public_stream = subscribe(&p, &public, Role::Public);
    p.start_route(1, Source::RemoteMix, Route::Transcription, Language::En)
        .unwrap();
    next_ready(&mut private).await;
    let sender = backend.events.lock().unwrap()[0].clone();
    for (revision, text, final_fragment) in [
        (1, "partial", false),
        (2, "partial sentence", false),
        (3, "", true),
    ] {
        sender
            .send(StreamEvent::Transcript {
                provenance: engine::Provenance::SourceAsr,
                text: text.into(),
                final_fragment,
                item: Some(engine::TranscriptItem {
                    source_audio: None,
                    id: "utterance_1".into(),
                    revision,
                    sequence: 1,
                }),
            })
            .await
            .unwrap();
        let event = private.events.recv().await.unwrap();
        assert_eq!(event["type"], "transcript");
        assert_eq!(
            event["item"],
            json!({"id":"utterance_1","revision":revision,"sequence":1})
        );
        assert_eq!(event["text"], text);
        assert_eq!(event["final"], final_fragment);
    }
    assert!(public_stream.events.try_recv().is_err());
    sender
        .send(StreamEvent::Transcript {
            provenance: engine::Provenance::SourceAsr,
            text: "resurrect".into(),
            final_fragment: true,
            item: Some(engine::TranscriptItem {
                source_audio: None,
                id: "utterance_1".into(),
                revision: 4,
                sequence: 1,
            }),
        })
        .await
        .unwrap();
    let denied = private.events.recv().await.unwrap();
    assert_ne!(denied["type"], "transcript");
    assert!(!denied.to_string().contains("resurrect"));
    assert!(public_stream.events.try_recv().is_err());
}

#[path = "moderation_tests.rs"]
mod moderation_tests;
