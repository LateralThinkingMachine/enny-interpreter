use super::*;
use crate::{config::ProviderActivation, session_host::SessionHost};
use axum::{
    Router,
    body::{Body, to_bytes},
    extract::{State, WebSocketUpgrade},
    http::{HeaderMap, Request, StatusCode},
    response::IntoResponse,
    routing::{get, post},
};
use futures_util::StreamExt;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicBool, AtomicUsize, Ordering},
};
use tower::ServiceExt;
fn activation() -> ProviderActivation {
    serde_json::from_value(json!({"api_key":"fixture-only-no-real-key","continuous":{"route_duration_seconds":300,
    "max_concurrent_sessions":2,"max_input_audio_seconds":600,"max_output_audio_seconds":600,"max_text_characters":76800,
    "warning_threshold_microusd":5000000,"allowed_routes":["transcription","live"],"allowed_live_modes":["interpretation"]}})).unwrap()
}
fn offer() -> Offer {
    serde_json::from_value(json!({"generation":1,"request_id":"a".repeat(64),"sdp":"v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\nm=application 9 UDP/DTLS/SCTP webrtc-datachannel\r\n",
    "interpretation":{"schema_version":"interpretation.v1","mode":"auto_to_target","target":"ja"},"participant_agreement":true,"headphones":true})).unwrap()
}
fn body(offer: &Offer) -> Value {
    json!({"generation":offer.generation,"request_id":offer.request_id,"sdp":offer.sdp,"interpretation":offer.interpretation,"voice":offer.voice,"participant_agreement":offer.participant_agreement,"headphones":offer.headphones})
}
#[derive(Clone)]
struct Fixture {
    posts: Arc<AtomicUsize>,
    seen: Arc<Mutex<Value>>,
    closed: bool,
    delay: bool,
    malformed: Arc<AtomicBool>,
    operator: Arc<Mutex<Option<crate::runtime::Subscription>>>,
}
async fn create(
    State(f): State<Fixture>,
    axum::Json(body): axum::Json<Value>,
) -> impl IntoResponse {
    f.posts.fetch_add(1, Ordering::SeqCst);
    *f.seen.lock().unwrap() = body;
    if f.delay {
        tokio::time::sleep(Duration::from_millis(150)).await;
    }
    (
        StatusCode::CREATED,
        axum::Json(
            json!({"session":{"id":"live_fixture"},"transport":{"type":"webrtc","sdp":if f.malformed.load(Ordering::SeqCst){"invalid answer"}else{"v=0\r\nm=audio 9 UDP/TLS/RTP/SAVPF 111\r\n"}}}),
        ),
    )
}
async fn attach(State(f): State<Fixture>, ws: WebSocketUpgrade) -> impl IntoResponse {
    ws.on_upgrade(move|mut socket|async move{
        use axum::extract::ws::Message;
        socket.send(Message::Text(json!({"type":"session.started","session":{"id":"live_fixture","model":"gpt-live-1"}}).to_string().into())).await.unwrap();
        while let Some(Ok(message))=socket.next().await {
            if let Message::Text(t)=message {
                let v:Value=serde_json::from_str(&t).unwrap();
                assert_eq!(v["type"],"session.close","attach must never session.start again");
                if f.closed {let _=socket.send(Message::Text(json!({"type":"session.closed","session":{"id":"live_fixture","model":"gpt-live-1"},"usage":{"seconds":1.5},"reason":"close_requested"}).to_string().into())).await;}
                break;
            }
        }
    })
}
async fn fixture(
    closed: bool,
    delay: bool,
) -> (
    Arc<SessionHost>,
    HeaderMap,
    Fixture,
    tokio::task::JoinHandle<()>,
) {
    fixture_with_operator(closed, delay, true).await
}
async fn fixture_with_operator(
    closed: bool,
    delay: bool,
    operator: bool,
) -> (
    Arc<SessionHost>,
    HeaderMap,
    Fixture,
    tokio::task::JoinHandle<()>,
) {
    let f = Fixture {
        posts: Arc::new(AtomicUsize::new(0)),
        seen: Arc::new(Mutex::new(Value::Null)),
        closed,
        delay,
        malformed: Arc::new(AtomicBool::new(false)),
        operator: Arc::new(Mutex::new(None)),
    };
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    let routes = Router::new()
        .route("/live", post(create))
        .route("/live/live_fixture/attach", get(attach))
        .with_state(f.clone());
    let task = tokio::spawn(async move {
        axum::serve(listener, routes).await.unwrap();
    });
    let (mut host, ticket) = SessionHost::new(
        Some(activation()),
        None,
        600,
        auth::LocalOrigins::default(),
        PathBuf::from("/nonexistent"),
        true,
    )
    .unwrap();
    let rtc = Arc::get_mut(&mut Arc::get_mut(&mut host).unwrap().webrtc).unwrap();
    rtc.provider = Some(Arc::new(transport::Provider::fixture(
        format!("http://{addr}/live"),
        format!("ws://{addr}/live"),
    )));
    let (view, token) = host.pilot().redeem(&ticket, Role::Operator).unwrap();
    let mut h = HeaderMap::new();
    h.insert("authorization", format!("Bearer {token}").parse().unwrap());
    h.insert(
        "x-pilot-csrf",
        view["csrf"].as_str().unwrap().parse().unwrap(),
    );
    if operator {
        let pilot = host.pilot();
        let authority = pilot.authorize(&h, Role::Operator, true).unwrap();
        *f.operator.lock().unwrap() = Some(pilot.subscribe(authority, Role::Operator).unwrap());
    }
    (host, h, f, task)
}
async fn request(
    host: Arc<SessionHost>,
    headers: &HeaderMap,
    path: &str,
    body: Value,
) -> (StatusCode, Value) {
    let method = if path.ends_with("/status") {
        "GET"
    } else {
        "POST"
    };
    let mut r = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host.pilot().origins().host(Role::Operator))
        .header("origin", host.pilot().origins().origin(Role::Operator))
        .header("content-type", "application/json");
    for (k, v) in headers {
        r = r.header(k, v);
    }
    let r = r
        .body(if method == "GET" {
            Body::empty()
        } else {
            Body::from(body.to_string())
        })
        .unwrap();
    let response = crate::http::hosted_router(host, Role::Operator)
        .oneshot(r)
        .await
        .unwrap();
    let status = response.status();
    let value =
        serde_json::from_slice(&to_bytes(response.into_body(), 64 * 1024).await.unwrap()).unwrap();
    (status, value)
}
#[tokio::test]
async fn actual_http_offer_sideband_stop_and_exact_retry_are_owned() {
    let (host, h, f, task) = fixture(true, false).await;
    let pilot = host.pilot();
    let input = offer();
    let mut legacy = body(&input);
    legacy.as_object_mut().unwrap().remove("voice");
    let (code, answer) = request(host.clone(), &h, "/api/webrtc/offer", legacy).await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(answer["session"]["id"], "live_fixture");
    assert_eq!(answer["requested_voice"], "marin");
    let sent = f.seen.lock().unwrap().clone();
    assert_eq!(sent["session"]["model"], "gpt-live-1");
    assert_eq!(sent["session"]["audio"]["output"]["voice"], "marin");
    assert_eq!(
        sent["session"]["instructions"],
        input.interpretation.instructions()
    );
    assert!(
        sent["session"]["instructions"]
            .as_str()
            .unwrap()
            .contains(crate::interpretation::INTERPRETER_CONVERSATION_POLICY_V3)
    );
    assert!(
        sent["session"]["instructions"]
            .as_str()
            .unwrap()
            .contains(crate::interpretation::INTERPRETER_ONSET_POLICY_V1)
    );
    assert!(
        sent["session"]["instructions"]
            .as_str()
            .unwrap()
            .contains(crate::interpretation::JAPANESE_INTERPRETER_FLOW_V1)
    );
    assert!(sent["session"]["audio"]["format"].is_null());
    assert_eq!(
        sent["session"]["audio"],
        json!({"output":{"voice":"marin"}})
    );
    assert_eq!(sent["session"]["store"], false);
    assert_eq!(
        sent["session"]["client"]["data_channel"]["allowed_client_events"],
        json!(["session.close"])
    );
    assert!(!answer.to_string().contains("fixture-only-no-real-key"));
    assert!(pilot.positive(1).is_err());
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&input))
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    let mut changed = body(&input);
    changed["interpretation"]["target"] = json!("en");
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", changed)
            .await
            .0,
        StatusCode::CONFLICT
    );
    request(
        host.clone(),
        &h,
        "/api/webrtc/stop",
        json!({"generation":1,"request_id":input.request_id}),
    )
    .await;
    assert!(host.webrtc.shutdown().await);
    assert!(pilot.positive(1).is_ok());
    let status = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(status["state"], "closed");
    assert_eq!(status["requested_voice"], "marin");
    assert_eq!(status["usage_seconds"], 1.5);
    assert_eq!(status["usage_confirmed"], true);
    assert_eq!(status["allocated_microusd"], 279167);
    let mut successor = body(&input);
    successor["request_id"] = json!("b".repeat(64));
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", successor)
            .await
            .0,
        StatusCode::OK
    );
    assert_eq!(
        request(
            host.clone(),
            &h,
            "/api/webrtc/stop",
            json!({"generation":1,"request_id":input.request_id})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    assert_eq!(
        f.seen.lock().unwrap()["session"],
        sent["session"],
        "a confirmed-closure successor retains the reviewed prompt and permissions"
    );
    let status = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(status["closure_receipts"].as_array().unwrap().len(), 1);
    assert_eq!(status["closure_receipts"][0]["usage_seconds"], 1.5);
    assert_eq!(status["allocated_microusd"], 558334);
    assert!(host.shutdown().await);
    task.abort();
}
#[tokio::test]
async fn builtin_voice_is_sent_frozen_and_retained_only_for_its_owned_window() {
    let (host, h, f, task) = fixture(true, false).await;
    let ready = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert!(ready.get("requested_voice").is_none());
    let voices = [
        InterpretationVoice::Marin,
        InterpretationVoice::Quartz,
        InterpretationVoice::Gleam,
        InterpretationVoice::Meridian,
    ];
    for (index, voice) in voices.iter().copied().enumerate() {
        let mut input = offer();
        input.request_id = format!("{:064x}", index + 100);
        input.voice = voice;
        let (code, answer) = request(host.clone(), &h, "/api/webrtc/offer", body(&input)).await;
        assert_eq!(code, StatusCode::OK);
        assert_eq!(answer["requested_voice"], voice.as_str());
        assert!(
            answer.get("effective_voice").is_none(),
            "a requested voice is not a provider voice receipt"
        );
        assert_eq!(
            f.seen.lock().unwrap()["session"]["audio"]["output"]["voice"],
            voice.as_str()
        );
        assert_eq!(
            request(host.clone(), &h, "/api/webrtc/offer", body(&input))
                .await
                .0,
            StatusCode::OK
        );
        let mut changed = body(&input);
        changed["voice"] = json!(voices[(index + 1) % voices.len()]);
        assert_eq!(
            request(host.clone(), &h, "/api/webrtc/offer", changed.clone())
                .await
                .0,
            StatusCode::CONFLICT
        );
        changed["request_id"] = json!(format!("{:064x}", index + 200));
        assert_eq!(
            request(host.clone(), &h, "/api/webrtc/offer", changed)
                .await
                .0,
            StatusCode::CONFLICT
        );
        assert_eq!(f.posts.load(Ordering::SeqCst), index + 1);
        let status = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
            .await
            .1;
        assert_eq!(status["requested_voice"], voice.as_str());
        assert_eq!(status["closure_receipts"].as_array().unwrap().len(), index);
        if index > 0 {
            assert_eq!(
                status["closure_receipts"][index - 1]["requested_voice"],
                voices[index - 1].as_str()
            );
        }
        let other = host.webrtc.status(&host.pilot(), "not-the-owner");
        assert_eq!(other["state"], "busy");
        assert!(other.get("requested_voice").is_none());
        assert!(host.webrtc.shutdown().await);
        let closed = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
            .await
            .1;
        assert_eq!(closed["closure_confirmed"], true);
        assert_eq!(closed["requested_voice"], voice.as_str());
        assert_eq!(
            request(host.clone(), &h, "/api/webrtc/offer", body(&input))
                .await
                .0,
            StatusCode::CONFLICT
        );
    }
    assert!(host.shutdown().await);
    task.abort();
}
#[tokio::test]
async fn unknown_or_nonstring_voice_is_rejected_before_provider_and_allocation() {
    let (host, h, f, task) = fixture(true, false).await;
    for voice in [
        Value::Null,
        json!(true),
        json!(1),
        json!([]),
        json!({"marin":null}),
        json!({"id":"custom_voice"}),
        json!("custom_voice"),
        json!("Marin"),
        json!(""),
    ] {
        let mut input = body(&offer());
        input["voice"] = voice;
        assert_eq!(
            request(host.clone(), &h, "/api/webrtc/offer", input)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    for field in ["instructions", "audio", "voice_id"] {
        let mut input = body(&offer());
        input[field] = json!("unreviewed");
        assert_eq!(
            request(host.clone(), &h, "/api/webrtc/offer", input)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    let status = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(status["allocated_microusd"], 0);
    assert!(status.get("requested_voice").is_none());
    assert!(host.shutdown().await);
    task.abort();
}
#[tokio::test]
async fn stop_during_creation_still_attaches_and_closes_without_releasing_answer() {
    let (host, h, f, task) = fixture(true, true).await;
    let input = offer();
    let h2 = h.clone();
    let host2 = host.clone();
    let create =
        tokio::spawn(async move { request(host2, &h2, "/api/webrtc/offer", body(&input)).await });
    while f.posts.load(Ordering::SeqCst) == 0 {
        tokio::task::yield_now().await;
    }
    host.pilot().stop("operator_stop");
    let before_close = host.pilot().stop_receipt(&h).unwrap();
    assert_eq!(before_close["provider_activity"], "unconfirmed");
    assert!(before_close["reserved_microusd"].is_null());
    assert_ne!(create.await.unwrap().0, StatusCode::OK);
    assert!(host.webrtc.shutdown().await);
    // The direct peer's final usage does not settle the whole meeting.
    assert_eq!(host.pilot().stop_receipt(&h).unwrap(), before_close);
    assert!(host.pilot().is_stopped());
    task.abort();
}
#[tokio::test]
async fn stop_before_offer_admission_tombstones_request_without_creating_voice() {
    let (host, h, f, task) = fixture(true, false).await;
    let input = offer();
    let (code, status) = request(
        host.clone(),
        &h,
        "/api/webrtc/stop",
        json!({"generation":1,"request_id":input.request_id}),
    )
    .await;
    assert_eq!(code, StatusCode::OK);
    assert_eq!(status["closure_confirmed"], true);
    assert_eq!(status["request_id"], input.request_id);
    assert_eq!(status["generation"], 1);
    assert_eq!(status["closure_basis"], "cancelled_before_admission");
    assert_eq!(status["usage_confirmed"], false);
    assert!(status["usage_seconds"].is_null());
    assert_eq!(
        request(
            host.clone(),
            &h,
            "/api/webrtc/stop",
            json!({"generation":1,"request_id":input.request_id})
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&input))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert_eq!(
        request(
            host.clone(),
            &h,
            "/api/webrtc/stop",
            json!({"generation":99,"request_id":"b".repeat(64)})
        )
        .await
        .0,
        StatusCode::CONFLICT
    );
    let mut next = body(&input);
    next["request_id"] = json!("b".repeat(64));
    next["voice"] = json!("quartz");
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", next).await.0,
        StatusCode::OK
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(host.shutdown().await);
    task.abort();
}
#[tokio::test]
async fn browser_offer_requires_existing_operator_websocket_owner() {
    let (host, h, f, task) = fixture_with_operator(true, false, false).await;
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&offer()))
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert!(host.shutdown().await);
    task.abort();
}
#[tokio::test]
async fn lost_final_usage_keeps_exclusive_fence_and_cannot_report_zero_cost() {
    let (host, h, _, task) = fixture(false, false).await;
    let input = offer();
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&input))
            .await
            .0,
        StatusCode::OK
    );
    assert!(!host.webrtc.shutdown().await);
    let status = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(status["state"], "closure_uncertain");
    assert_eq!(status["closure_confirmed"], false);
    assert_eq!(status["usage_confirmed"], false);
    assert_eq!(status["requested_voice"], "marin");
    let repeated_stop = request(
        host.clone(),
        &h,
        "/api/webrtc/stop",
        json!({"generation":1,"request_id":input.request_id}),
    )
    .await
    .1;
    assert_eq!(repeated_stop["state"], "closure_uncertain");
    assert!(host.pilot().positive(1).is_err());
    let mut next = body(&input);
    next["request_id"] = json!("b".repeat(64));
    next["voice"] = json!("quartz");
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", next).await.0,
        StatusCode::CONFLICT
    );
    task.abort();
}
#[tokio::test]
async fn admission_requires_auth_csrf_agreement_and_valid_audio_only_sdp() {
    let (host, h, f, task) = fixture(true, false).await;
    let input = offer();
    assert_eq!(
        request(
            host.clone(),
            &HeaderMap::new(),
            "/api/webrtc/offer",
            body(&input)
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
    let mut bad = h.clone();
    bad.remove("x-pilot-csrf");
    assert_eq!(
        request(host.clone(), &bad, "/api/webrtc/offer", body(&input))
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    let mut value = body(&input);
    value["participant_agreement"] = json!(false);
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", value)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut value = body(&input);
    value["sdp"] = json!(format!("{}m=video 9 RTP/AVP 100\r\n", input.sdp));
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", value)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    let mut value = body(&input);
    value["model"] = json!("other");
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", value)
            .await
            .0,
        StatusCode::BAD_REQUEST
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert!(host.shutdown().await);
    task.abort();
}
#[test]
fn offer_and_ducking_contract_do_not_change_asr_input_or_expose_settings() {
    let input = offer();
    let request = input.provider_body();
    assert!(request["session"]["audio"].get("input").is_none());
    assert!(request["session"].get("tools").is_none());
    assert_eq!(ducking()["ducked_gain"], 0.20);
    assert_eq!(ducking()["attack_ms"], 25);
    assert_eq!(ducking()["headroom"], 0.5);
    assert!(valid_request_id(&"a".repeat(64)));
    assert!(!valid_request_id(&"g".repeat(64)));
}
#[tokio::test]
async fn expired_browser_lease_retires_voice_without_reconnecting() {
    let (host, h, f, task) = fixture(true, false).await;
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&offer()))
            .await
            .0,
        StatusCode::OK
    );
    let owner = host.webrtc.state.lock().unwrap().owner.clone().unwrap();
    owner.observation.lock().unwrap().heartbeat = Instant::now() - Duration::from_secs(31);
    let mut done = owner.completed.subscribe();
    tokio::time::timeout(Duration::from_secs(2), async {
        while !*done.borrow() {
            done.changed().await.unwrap();
        }
    })
    .await
    .unwrap();
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(owner.observation.lock().unwrap().closure_confirmed);
    assert!(
        request(
            host.clone(),
            &h,
            "/api/webrtc/heartbeat",
            json!({"generation":1,"request_id":"a".repeat(64)})
        )
        .await
        .0
        .is_client_error()
    );
    assert!(host.shutdown().await);
    task.abort();
}
#[tokio::test]
async fn hostile_origin_public_surface_and_disabled_checkout_cannot_create_voice() {
    let (host, h, f, task) = fixture(true, false).await;
    for (role, origin, path) in [
        (Role::Operator, "https://other.example", "/api/webrtc/offer"),
        (Role::Public, auth::PUBLIC_ORIGIN, "/api/webrtc/status"),
        (
            Role::Operator,
            auth::ORIGIN,
            "/api/webrtc/status?secret=invalid",
        ),
    ] {
        let r = Request::builder()
            .method("POST")
            .uri(path)
            .header("host", host.pilot().origins().host(role))
            .header("origin", origin)
            .header("content-type", "application/json")
            .header("authorization", h["authorization"].clone())
            .header("x-pilot-csrf", h["x-pilot-csrf"].clone())
            .body(Body::from(body(&offer()).to_string()))
            .unwrap();
        let response = crate::http::hosted_router(host.clone(), role)
            .oneshot(r)
            .await
            .unwrap();
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
    }
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    assert!(host.shutdown().await);
    task.abort();
    let (host, ticket) = SessionHost::new(
        None,
        None,
        600,
        auth::LocalOrigins::default(),
        PathBuf::from("/nonexistent"),
        true,
    )
    .unwrap();
    let (view, token) = host.pilot().redeem(&ticket, Role::Operator).unwrap();
    let mut h = HeaderMap::new();
    h.insert("authorization", format!("Bearer {token}").parse().unwrap());
    h.insert(
        "x-pilot-csrf",
        view["csrf"].as_str().unwrap().parse().unwrap(),
    );
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/status", Value::Null)
            .await
            .1["enabled"],
        false
    );
    assert_eq!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&offer()))
            .await
            .0,
        StatusCode::SERVICE_UNAVAILABLE
    );
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn malformed_answer_still_closes_a_known_provider_session() {
    let (host, h, f, task) = fixture(true, false).await;
    f.malformed.store(true, Ordering::SeqCst);
    assert_ne!(
        request(host.clone(), &h, "/api/webrtc/offer", body(&offer()))
            .await
            .0,
        StatusCode::OK
    );
    assert!(host.webrtc.shutdown().await);
    let status = request(host.clone(), &h, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(status["closure_confirmed"], true);
    assert_eq!(status["issue"], "invalid_provider_answer");
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    assert!(host.shutdown().await);
    task.abort();
}

#[tokio::test]
async fn two_way_pair_reaches_provider_and_survives_retry_and_confirmed_successor() {
    let (host, headers, f, task) = fixture(true, false).await;
    let mut input = offer();
    input.interpretation = serde_json::from_value(json!({
        "schema_version":"interpretation.v1", "mode":"two_way", "first":"ja", "second":"en"
    }))
    .unwrap();
    let (code, answer) = request(host.clone(), &headers, "/api/webrtc/offer", body(&input)).await;
    assert_eq!(code, StatusCode::OK);
    let sent = f.seen.lock().unwrap().clone();
    assert_eq!(sent["session"]["model"], "gpt-live-1");
    assert_eq!(
        sent["session"]["instructions"],
        input.interpretation.instructions()
    );
    let instructions = sent["session"]["instructions"].as_str().unwrap();
    assert!(
        instructions.contains("Interpret Japanese into English; interpret English into Japanese")
    );
    assert!(instructions.contains("including a change within a turn or after a speaker change"));
    assert!(instructions.contains("do not wait for a full turn or ordinary silence"));
    assert!(instructions.contains(crate::interpretation::INTERPRETER_ONSET_POLICY_V1));
    assert!(!instructions.contains("If speech is already in"));
    assert!(instructions.contains("This session has no authority to publish to a meeting"));
    assert_eq!(sent["session"]["store"], false);
    assert_eq!(
        sent["session"]["client"]["data_channel"]["allowed_client_events"],
        json!(["session.close"])
    );
    assert_eq!(
        request(host.clone(), &headers, "/api/webrtc/offer", body(&input))
            .await
            .1,
        answer,
        "an exact pair retry returns the same answer without a second provider create"
    );
    for policy in [
        json!({"schema_version":"interpretation.v1","mode":"auto_to_target","target":"ja"}),
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"en","second":"ja"}),
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"ja","second":"es"}),
    ] {
        let mut changed = body(&input);
        changed["interpretation"] = policy;
        assert_eq!(
            request(host.clone(), &headers, "/api/webrtc/offer", changed)
                .await
                .0,
            StatusCode::CONFLICT,
            "retry identity binds the exact pair and mode"
        );
    }
    let mut successor = input.clone();
    successor.request_id = "b".repeat(64);
    assert_eq!(
        request(
            host.clone(),
            &headers,
            "/api/webrtc/offer",
            body(&successor)
        )
        .await
        .0,
        StatusCode::CONFLICT,
        "another pair window cannot overlap its predecessor"
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 1);
    request(
        host.clone(),
        &headers,
        "/api/webrtc/stop",
        json!({"generation":1,"request_id":input.request_id}),
    )
    .await;
    assert!(host.webrtc.shutdown().await);
    let closed = request(host.clone(), &headers, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(closed["closure_confirmed"], true);
    assert_eq!(closed["usage_confirmed"], true);
    assert_eq!(
        request(
            host.clone(),
            &headers,
            "/api/webrtc/offer",
            body(&successor)
        )
        .await
        .0,
        StatusCode::OK
    );
    assert_eq!(f.posts.load(Ordering::SeqCst), 2);
    assert_eq!(
        f.seen.lock().unwrap()["session"],
        sent["session"],
        "a controller's immutable pair produces the same provider configuration after confirmed closure"
    );
    assert!(host.shutdown().await);
    task.abort();
}

#[tokio::test]
async fn invalid_two_way_pair_is_rejected_before_provider_or_allocation() {
    let (host, headers, f, task) = fixture(true, false).await;
    for (index, policy) in [
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"ja","second":"ja"}),
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"ja","second":"unsupported"}),
        json!({"schema_version":"interpretation.v1","mode":"two_way","first":"ja","second":"en","target":"ja"}),
    ]
    .into_iter()
    .enumerate()
    {
        let mut input = body(&offer());
        input["request_id"] = json!(format!("{:064x}", 700 + index));
        input["interpretation"] = policy;
        assert_eq!(
            request(host.clone(), &headers, "/api/webrtc/offer", input)
                .await
                .0,
            StatusCode::BAD_REQUEST
        );
    }
    assert_eq!(f.posts.load(Ordering::SeqCst), 0);
    let state = request(host.clone(), &headers, "/api/webrtc/status", Value::Null)
        .await
        .1;
    assert_eq!(state["allocated_microusd"], 0);
    assert_eq!(state["closure_confirmed"], true);
    assert!(host.shutdown().await);
    task.abort();
}
