//! Follow an operator action through Axum, the finite vendor dispatcher, real
//! Gemini protocol and reservation ledger. Only the socket dial is in-memory.
use super::*;
use crate::{
    Pilot, auth,
    config::{ActivatedBackend, ProviderActivation},
    runtime::Subscription,
};
use axum::{
    Router,
    body::{Body, to_bytes},
    http::{HeaderMap, Method, Request as HttpRequest, StatusCode},
};
use tower::ServiceExt;

const SESSION: u64 = 13;

struct Meeting {
    backend: Arc<FiniteBackend>,
    pilot: Arc<Pilot>,
    app: Router,
    auth: HeaderMap,
    subscription: Subscription,
    peers: mpsc::Receiver<(Route, WebSocketStream<DuplexStream>)>,
}

impl Meeting {
    fn new() -> Self {
        let authorization: ProviderActivation = serde_json::from_value(json!({
            "vendor":"gemini","api_key":"fictional-never-transmitted",
            "allowed_routes":["transcription","live"],"allowed_live_modes":["interpretation"],
            "allow_tts":false,"max_tts_requests":0,"total_duration_seconds":120,
            "route_duration_seconds":60,"max_sessions":2,"max_input_audio_seconds":180,
            "max_output_audio_seconds":180,"max_text_characters":3000,"approved_budget_microusd":200000
        })).unwrap();
        let ActivatedBackend::Finite(mut backend) = authorization.into_backend(Some(120)).unwrap()
        else {
            panic!("finite authorization required");
        };
        let (send, peers) = mpsc::channel(2);
        let shared = &mut Arc::get_mut(&mut backend).unwrap().shared;
        Arc::get_mut(shared).unwrap().gemini_peers = Some(send);
        let (pilot, ticket) = Pilot::new_hosted(
            backend.clone(),
            Some(Duration::from_secs(120)),
            180,
            auth::LocalOrigins::default(),
            SESSION,
            false,
            Instant::now(),
        )
        .unwrap();
        let (view, token) = pilot.redeem(&ticket, auth::Role::Operator).unwrap();
        assert_eq!(view["provider_vendor"], "gemini");
        assert_eq!(view["generation"], SESSION);
        let mut auth = HeaderMap::new();
        auth.insert("authorization", format!("Bearer {token}").parse().unwrap());
        auth.insert(
            "x-pilot-csrf",
            view["csrf"].as_str().unwrap().parse().unwrap(),
        );
        let session = pilot.authorize(&auth, auth::Role::Operator, false).unwrap();
        let subscription = pilot.subscribe(session, auth::Role::Operator).unwrap();
        let app = crate::router(pilot.clone(), Default::default());
        Self {
            backend,
            pilot,
            app,
            auth,
            subscription,
            peers,
        }
    }

    async fn request(&self, path: &str, json: Option<Value>) -> (StatusCode, Value) {
        let mut request = HttpRequest::builder()
            .uri(path)
            .method(if json.is_some() {
                Method::POST
            } else {
                Method::GET
            })
            .header("host", auth::HOST)
            .header("origin", auth::ORIGIN)
            .header("authorization", &self.auth["authorization"])
            .header("x-pilot-csrf", &self.auth["x-pilot-csrf"]);
        let body = match json {
            Some(json) => {
                request = request.header("content-type", "application/json");
                Body::from(json.to_string())
            }
            None => Body::empty(),
        };
        let response = self
            .app
            .clone()
            .oneshot(request.body(body).unwrap())
            .await
            .unwrap();
        let status = response.status();
        let bytes = to_bytes(response.into_body(), 4096).await.unwrap();
        (status, serde_json::from_slice(&bytes).unwrap())
    }

    async fn open(&mut self, route: Route, model: &str) -> WebSocketStream<DuplexStream> {
        let mut action =
            json!({"generation":SESSION,"source":"remote_mix","language":"ja","route":route});
        if route == Route::Live {
            action["live_mode"] = json!("interpretation");
        }
        let (status, accepted) = self.request("/api/routes", Some(action)).await;
        assert_eq!(status, StatusCode::ACCEPTED, "{accepted}");
        let (dispatched, mut socket) = self.peers.recv().await.unwrap();
        assert_eq!(dispatched, route);
        assert_eq!(read_json(&mut socket).await["setup"]["model"], model);
        send_binary_json(&mut socket, json!({"setupComplete":{}})).await;
        loop {
            let event = self.subscription.events.recv().await.unwrap();
            if event["type"] == "route"
                && event["route_id"] == accepted["route_id"]
                && event["state"] == "ready"
            {
                assert_eq!(event["generation"], SESSION);
                break;
            }
        }
        socket
    }

    async fn stop(&self) {
        let (status, stopped) = self.request("/api/stop", Some(json!({}))).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(stopped, json!({"state":"stopped","generation":SESSION + 1}));
    }

    async fn assert_receipt(&self, activity: &str, reserved: u64) {
        let (status, receipt) = self.request("/api/stop-receipt", None).await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(
            receipt,
            json!({"version":1,"session_generation":SESSION,"generation":SESSION + 1,
            "provider_activity":activity,"reserved_microusd":reserved,"invoice":"unavailable"})
        );
    }
}

#[tokio::test]
async fn finite_gemini_stop_receipt_with_no_admitted_routes() {
    let meeting = Meeting::new();
    assert_eq!(
        meeting.request("/api/stop-receipt", None).await.0,
        StatusCode::CONFLICT
    );
    meeting.stop().await;
    meeting.assert_receipt("none_admitted", 0).await;
    assert!(meeting.backend.diagnostics().is_empty());
}

async fn run_two_routes(confirm_second: bool) {
    let mut meeting = Meeting::new();
    let mut source = meeting
        .open(Route::Transcription, "models/gemini-3.5-transcribe-live")
        .await;
    let mut translation = meeting
        .open(Route::Live, "models/gemini-3.5-live-translate-preview")
        .await;
    // One synthetic packet of 100 ms is fanned out by the native route owner.
    for sequence in 0_u32..5 {
        let mut frame = [0x31; 646];
        frame[..2].copy_from_slice(&[1, 0]);
        frame[2..6].copy_from_slice(&sequence.to_le_bytes());
        meeting.pilot.frame(&frame).unwrap();
    }
    for socket in [&mut source, &mut translation] {
        let packet = read_json(socket).await;
        let audio = &packet["realtimeInput"]["audio"];
        assert_eq!(audio["mimeType"], "audio/pcm;rate=16000");
        assert_eq!(
            STANDARD.decode(audio["data"].as_str().unwrap()).unwrap(),
            vec![0x31; 3200]
        );
    }

    meeting.stop().await;
    for socket in [&mut source, &mut translation] {
        assert!(matches!(socket.next().await, Some(Ok(Message::Close(_)))));
    }
    meeting.assert_receipt("unconfirmed", 200_000).await;
    source.flush().await.unwrap(); // deliver the first peer's close frame
    while meeting.backend.diagnostics().is_empty() {
        tokio::task::yield_now().await;
    }
    assert!(meeting.backend.diagnostics()[0].closed_confirmed);
    meeting.assert_receipt("unconfirmed", 200_000).await;
    if confirm_second {
        translation.flush().await.unwrap();
    }
    drop(translation); // an unflushed Close response is EOF on the client
    assert!(meeting.backend.retire().await);
    assert_eq!(meeting.backend.diagnostics().len(), 2);
    let expected = if confirm_second {
        "close_observed"
    } else {
        "unconfirmed"
    };
    meeting.assert_receipt(expected, 200_000).await;
}

#[tokio::test]
async fn finite_gemini_stop_receipt_requires_peer_close_for_both_admitted_routes() {
    for confirm_second in [true, false] {
        tokio::time::timeout(Duration::from_secs(5), run_two_routes(confirm_second))
            .await
            .expect("both native Gemini transports must retire");
    }
}
