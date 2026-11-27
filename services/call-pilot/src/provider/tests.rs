use super::*;
#[path = "budget_tests.rs"]
mod budget_tests;
#[path = "drain_tests.rs"]
mod drain_tests;
#[path = "interpretation_tests.rs"]
mod interpretation_tests;
#[path = "live_drain_tests.rs"]
mod live_drain_tests;
#[path = "live_duplex_tests.rs"]
mod live_duplex_tests;
#[path = "live_termination_tests.rs"]
mod live_termination_tests;
#[path = "manual_tests.rs"]
mod manual_tests;
#[path = "revocation_tests.rs"]
mod revocation_tests;
#[path = "startup_fence_tests.rs"]
mod startup_fence_tests;
#[path = "stop_receipt_tests.rs"]
mod stop_receipt_tests;
#[path = "text_budget_tests.rs"]
mod text_budget_tests;
use crate::engine::{Provenance, StreamEvent};
use base64::{Engine, engine::general_purpose::STANDARD};
use serde_json::{Value, json};
use std::{
    future::Future,
    task::{Context, Poll, Wake, Waker},
    time::{SystemTime, UNIX_EPOCH},
};

fn config() -> ProviderConfig {
    ProviderConfig {
        api_key: "synthetic-not-a-real-key".into(),
        allowed_routes: vec![Route::Transcription, Route::Translation, Route::Live],
        allowed_live_modes: vec![crate::engine::LiveMode::Assistant],
        allow_tts: true,
        total_duration_seconds: 120,
        route_duration_seconds: 60,
        max_sessions: 6,
        max_tts_requests: 2,
        max_input_audio_seconds: 240,
        max_output_audio_seconds: 240,
        max_text_characters: 20000,
        approved_budget_microusd: 100_000_000,
    }
}

#[test]
fn gemini_duration_bounds_both_route_quote_and_atomic_admission() {
    let config_for = |total, route| ProviderConfig {
        total_duration_seconds: total,
        route_duration_seconds: route,
        allowed_routes: vec![Route::Transcription, Route::Live],
        allowed_live_modes: vec![crate::engine::LiveMode::Interpretation],
        allow_tts: false,
        max_tts_requests: 0,
        ..config()
    };
    use crate::budget::ProviderVendor;
    let finite =
        super::FiniteBackend::for_vendor(config_for(600, 600), ProviderVendor::Gemini).unwrap();
    for route in [Route::Transcription, Route::Live] {
        let quote = finite
            .shared
            .budget_snapshot()
            .routes
            .into_iter()
            .find(|q| q.route == route)
            .unwrap();
        assert!(quote.can_reserve && quote.quote_seconds.is_some_and(|s| (1..=600).contains(&s)));
        let mut reservation = finite.shared.reserve(Some(route), 0).unwrap();
        assert!((1..=600).contains(&reservation.seconds));
        reservation.completed = true;
    }
    for (total, route) in [(600, 601), (601, 600), (601, 601)] {
        assert!(
            super::FiniteBackend::for_vendor(config_for(total, route), ProviderVendor::Gemini)
                .is_err()
        );
    }
    assert!(super::OpenAiBackend::new(config_for(601, 601)).is_ok());
}

#[test]
fn diagnostics_keep_closed_metadata_and_protocol_guards() {
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    let mut s = resolved(Route::Transcription);
    s.as_object_mut().unwrap().remove("id");
    assert!(
        p.consume(json!({"type":"session.created","session":s}))
            .is_err()
    );
    assert!(!p.ready);
    assert_eq!(
        p.failure,
        Some((
            DiagnosticEvent::SessionCreated,
            DiagnosticCategory::MissingSessionId
        ))
    );
    // Unknown upstream strings and even error messages resembling keys remain
    // untrusted values. No value is copied into a diagnostic.
    let canary = "synthetic-private-canary-do-not-record";
    let mut p = ready(Route::Live);
    assert!(
        p.consume(
            json!({"type":"error","message":canary,"error":{"message":canary,"code":canary}})
        )
        .is_err()
    );
    assert_eq!(
        p.failure,
        Some((DiagnosticEvent::Error, DiagnosticCategory::UpstreamError))
    );
    assert!(p.consume(json!({"type":canary,"delta":canary})).is_err());
    assert_eq!(
        p.failure,
        Some((DiagnosticEvent::Error, DiagnosticCategory::UpstreamError))
    );
    let (event, category) = diagnostics::rejected_event(Route::Live, true, &json!({"type":canary}));
    assert_eq!(
        (event, category),
        (DiagnosticEvent::Other, DiagnosticCategory::UnsupportedEvent)
    );
}

#[test]
fn handshake_diagnostic_never_serializes_response_contents() {
    use tokio_tungstenite::tungstenite::{Error, http::Response};
    let canary = "synthetic-private-canary-do-not-record";
    let error = Error::Http(Box::new(
        Response::builder()
            .status(400)
            .header("x-private", canary)
            .body(Some(canary.as_bytes().to_vec()))
            .unwrap(),
    ));
    let d = ProviderDiagnostic {
        route: Route::Transcription,
        stage: DiagnosticStage::Handshake,
        category: DiagnosticCategory::HttpRefused,
        event_type: DiagnosticEvent::None,
        upstream_code: DiagnosticCode::None,
        upstream_parameter: DiagnosticParameter::None,
        http_status: transport::handshake_status(&error),
        ready: false,
        closed_confirmed: false,
    };
    let result = serde_json::to_value(d).unwrap();
    assert_eq!(result["http_status"], 400);
    assert_eq!(result.as_object().unwrap().len(), 9);
    assert!(!result.to_string().contains(canary));
    assert_eq!(transport::handshake_status(&Error::ConnectionClosed), None);
}

#[test]
fn diagnostic_storage_is_bounded_and_snapshot_is_detached() {
    let backend = OpenAiBackend::new(config()).unwrap();
    for _ in 0..20 {
        backend.shared.record_diagnostic(ProviderDiagnostic {
            route: Route::Live,
            stage: DiagnosticStage::Complete,
            category: DiagnosticCategory::Confirmed,
            event_type: DiagnosticEvent::None,
            upstream_code: DiagnosticCode::None,
            upstream_parameter: DiagnosticParameter::None,
            http_status: None,
            ready: true,
            closed_confirmed: true,
        });
    }
    let mut snapshot = backend.diagnostics();
    assert_eq!(snapshot.len(), 16);
    snapshot.clear();
    assert_eq!(backend.diagnostics().len(), 16);
}
fn resolved(route: Route) -> Value {
    let expiry = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_secs()
        + 3600;
    match route {
        Route::Live => {
            json!({"id":"sess_synthetic","model":protocol::LIVE,"status":"active","expires_at":expiry,"store":false,"delegation":{"type":"client"},"audio":{"format":{"type":"audio/pcm","rate":16000},"output":{"voice":"marin"}}})
        }
        Route::Transcription => {
            json!({"id":"sess_synthetic","object":"realtime.transcription_session","type":"transcription","expires_at":expiry,"audio":{"input":{"format":{"type":"audio/pcm","rate":24000},"transcription":{"model":protocol::ASR,"languages":["ja","en","hi"],"delay":"low"},"turn_detection":null}}})
        }
        Route::Translation => {
            json!({"id":"sess_synthetic","type":"translation","model":protocol::TRANSLATE,"expires_at":expiry,"audio":{"input":{"transcription":null},"output":{"language":"ja"}}})
        }
    }
}
fn ready(route: Route) -> protocol::Protocol {
    let mut p = protocol::Protocol::new(route, Language::Ja);
    // Historical server-VAD item traces remain useful ledger regressions. The
    // network path always constructs Protocol::new with the local ledger.
    if route == Route::Transcription {
        p.asr = asr::Asr::default();
    }
    if route != Route::Live {
        p.consume(json!({"type":"session.created","session":resolved(route)}))
            .unwrap();
    }
    assert_eq!(p.consume(json!({"type":if route==Route::Live{"session.started"}else{"session.updated"},"session":resolved(route)})).unwrap().len(),1);
    p
}
#[test]
fn startup_confirms_exact_models_modes_and_no_audio_before_ready() {
    for route in [Route::Live, Route::Translation, Route::Transcription] {
        let mut p = protocol::Protocol::new(route, Language::Ja);
        assert!(
            p.consume(json!({"type":"session.output_audio.delta","delta":"AAAA"}))
                .is_err()
        );
        let mut session = resolved(route);
        if route == Route::Transcription {
            session["audio"]["input"]["transcription"]["model"] = json!("whisper-1")
        } else {
            session["model"] = json!("gpt-realtime")
        }
        if route != Route::Live {
            p.consume(json!({"type":"session.created","session":resolved(route)}))
                .unwrap();
        }
        assert!(p.consume(json!({"type":if route==Route::Live{"session.started"}else{"session.updated"},"session":session})).is_err());
        assert!(!p.ready);
    }
    let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
    let mut s = resolved(Route::Live);
    s["delegation"] = json!({"type":"responses"});
    assert!(
        p.consume(json!({"type":"session.started","session":s}))
            .is_err()
    );
    assert_eq!(p.id.as_deref(), Some("sess_synthetic"));
}
#[test]
fn startup_rejects_wrong_language_expiry_and_store() {
    let mut p = protocol::Protocol::new(Route::Translation, Language::Hi);
    p.consume(json!({"type":"session.created","session":resolved(Route::Translation)}))
        .unwrap();
    assert!(
        p.consume(json!({"type":"session.updated","session":resolved(Route::Translation)}))
            .is_err()
    );
    for key in ["expires_at", "store"] {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        let mut s = resolved(Route::Live);
        s[key] = if key == "store" {
            json!(true)
        } else {
            json!(0)
        };
        assert!(
            p.consume(json!({"type":"session.started","session":s}))
                .is_err()
        );
    }
}

#[test]
fn asr_optional_expiry_is_absence_only_and_never_invents_a_provider_deadline() {
    let mut session = resolved(Route::Transcription);
    session.as_object_mut().unwrap().remove("expires_at");
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    p.consume(json!({"type":"session.created","session":session}))
        .unwrap();
    assert!(p.expiry.is_none());
    p.consume(json!({"type":"session.updated","session":session}))
        .unwrap();
    assert!(p.ready);
    assert!(p.expiry.is_none());
    let mut missing_model = session.clone();
    missing_model["audio"]["input"]["transcription"]
        .as_object_mut()
        .unwrap()
        .remove("model");
    assert_eq!(
        diagnostics::rejected_event(
            Route::Transcription,
            false,
            &json!({"type":"session.updated","session":missing_model})
        ),
        (
            DiagnosticEvent::SessionUpdated,
            DiagnosticCategory::MissingModel
        )
    );
    for bad in [
        Value::Null,
        json!(0),
        json!(-1),
        json!("future"),
        json!(true),
    ] {
        let mut invalid = session.clone();
        invalid["expires_at"] = bad;
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        assert!(
            p.consume(json!({"type":"session.created","session":invalid}))
                .is_err()
        );
        assert!(!p.ready);
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        p.consume(json!({"type":"session.created","session":session}))
            .unwrap();
        assert!(
            p.consume(json!({"type":"session.updated","session":invalid}))
                .is_err()
        );
        assert!(!p.ready);
    }
    for route in [Route::Live, Route::Translation] {
        let mut s = resolved(route);
        s.as_object_mut().unwrap().remove("expires_at");
        let mut p = protocol::Protocol::new(route, Language::Ja);
        assert!(p.consume(json!({"type":if route==Route::Live {"session.started"} else {"session.created"},"session":s})).is_err());
        assert!(!p.ready);
    }
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    p.consume(json!({"type":"session.created","session":resolved(Route::Transcription)}))
        .unwrap();
    let earlier = p.expiry;
    p.consume(json!({"type":"session.updated","session":session}))
        .unwrap();
    assert_eq!(p.expiry, earlier);
}

#[tokio::test]
async fn asr_missing_provider_expiry_ready_uses_bounded_local_authority() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (_input, input) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        let result = transport::drive(
            &shared,
            &mut client,
            &mut p,
            input,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert!(result.is_ok());
        assert!(p.ready && p.expiry.is_none());
    });
    let mut session = resolved(Route::Transcription);
    session.as_object_mut().unwrap().remove("expires_at");
    server
        .send(Message::Text(
            json!({"type":"session.created","session":session})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let request: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(request, protocol::start(Route::Transcription, Language::Ja));
    server
        .send(Message::Text(
            json!({"type":"session.updated","session":session})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Ready {
            expires_in_seconds: 1..=20
        })
    ));
    stop.send_replace(true);
    task.await.unwrap();
}
#[test]
fn outbound_schemas_keep_models_tools_and_context_separate() {
    let s = protocol::start(Route::Live, Language::Hi);
    assert_eq!(s["session"]["model"], "gpt-live-1");
    assert_eq!(s["session"]["delegation"], json!({"type":"client"}));
    assert_eq!(s["session"]["store"], false);
    let s = protocol::start(Route::Translation, Language::En);
    assert!(s["session"]["audio"]["input"]["transcription"].is_null());
    assert_eq!(s["session"]["audio"]["output"]["language"], "en");
    let mut p = ready(Route::Live);
    let command = p
        .live_text("untrusted instructions remain private data")
        .unwrap();
    assert!(command.get("text").is_none());
    assert!(command["content"].is_string());
    assert!(command["delegation_id"].is_null());
    assert!(p.live_text(&"x".repeat(401)).is_err());
    assert!(ready(Route::Translation).live_text("x").is_err());
}
#[test]
fn asr_final_items_are_ordered_and_deltas_do_not_duplicate_notes() {
    let mut p = ready(Route::Transcription);
    p.frames = 200;
    for id in ["item_a", "item_b"] {
        p.commit().unwrap();
        p.consume(json!({"type":"input_audio_buffer.committed","item_id":id}))
            .unwrap();
    }
    assert!(matches!(&p.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"item_a","content_index":0,"delta":"partial"})).unwrap()[0],StreamEvent::Transcript{final_fragment:false,item:Some(_),..}));
    let complete = |id: &str, transcript: &str| json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":transcript,"usage":{"type":"duration","seconds":2}});
    assert!(p.consume(complete("item_b", "second")).unwrap().is_empty());
    let output = p.consume(complete("item_a", "first")).unwrap();
    assert_eq!(output.len(), 2);
    for (event, expected) in output.iter().zip(["first", "second"]) {
        match event {
            StreamEvent::Transcript {
                provenance,
                text,
                final_fragment,
                ..
            } => {
                assert_eq!(*provenance, Provenance::SourceAsr);
                assert_eq!(text, expected);
                assert!(*final_fragment)
            }
            _ => panic!("wrong event"),
        }
    }
    assert!(p.complete_input());
    assert!(p.consume(complete("item_b", "duplicate")).is_err());
}
#[test]
fn asr_usage_pending_and_content_bounds_are_closed() {
    let mut p = ready(Route::Transcription);
    for _ in 0..8 {
        p.commit().unwrap();
    }
    assert!(p.commit().is_err());
    assert!(p.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"item_a","content_index":0,"delta":"x".repeat(4097)})).is_err());
    let mut p = ready(Route::Transcription);
    p.commit().unwrap();
    p.consume(json!({"type":"input_audio_buffer.committed","item_id":"item_a"}))
        .unwrap();
    assert!(p.consume(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"item_a","content_index":0,"transcript":"x","usage":{"type":"duration","seconds":1000}})).is_err());
}
#[test]
fn live_close_requires_exact_session_final_monotonic_usage() {
    let mut p = ready(Route::Live);
    p.consume(json!({"type":"session.usage.updated","usage":{"seconds":1.5}}))
        .unwrap();
    assert!(
        p.consume(json!({"type":"session.usage.updated","usage":{"seconds":1.0}}))
            .is_err()
    );
    assert!(p.consume(json!({"type":"session.closed","session":{"id":"other","model":protocol::LIVE},"reason":"close_requested","usage":{"seconds":2}})).is_err());
    assert!(!p.closed);
    p.consume(json!({"type":"session.closed","session":{"id":"sess_synthetic","model":protocol::LIVE},"reason":"close_requested","usage":{"seconds":2}})).unwrap();
    assert!(p.closed);
}
#[test]
fn translated_and_live_output_never_acquire_source_provenance() {
    let mut p = ready(Route::Translation);
    let v=p.consume(json!({"type":"session.output_transcript.delta","delta":"こんにちは ","elapsed_ms":200})).unwrap();
    assert!(matches!(
        &v[0],
        StreamEvent::Transcript {
            provenance: Provenance::Translation,
            final_fragment: false,
            ..
        }
    ));
    assert!(
        p.consume(json!({"type":"session.input_transcript.delta","delta":"source"}))
            .is_err()
    );
    p.consume(json!({"type":"session.closed","event_id":"closed"}))
        .unwrap();
    assert!(p.closed);
    let mut p = ready(Route::Live);
    let v=p.consume(json!({"type":"session.output_transcript.delta","delta":"私の提案","start_ms":1.25,"end_ms":2.75})).unwrap();
    assert!(matches!(
        &v[0],
        StreamEvent::LiveTranscript {
            provenance: Provenance::LiveOutput,
            start_ms: 1.25,
            end_ms: 2.75,
            ..
        }
    ));
}
#[test]
fn audio_format_and_frame_limits_reject_misdeclared_payloads() {
    let mut p = ready(Route::Translation);
    assert!(
        p.consume(json!({"type":"session.output_audio.delta","delta":"AAAA","sample_rate":16000}))
            .is_err()
    );
    let mut p = ready(Route::Live);
    assert!(
        p.consume(
            json!({"type":"session.output_audio.delta","delta":STANDARD.encode(vec![0u8;49154])})
        )
        .is_err()
    );
    assert!(
        p.consume(json!({"type":"session.output_audio.delta","delta":"AA=="}))
            .is_err()
    );
}
#[test]
fn reservations_are_shared_unrefunded_and_cancellation_is_sticky() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let mut a = backend.shared.reserve(Some(Route::Live), 0).unwrap();
    let cost = backend.shared.state.lock().unwrap().reserved;
    assert_eq!(cost, 50_000);
    a.completed = true;
    drop(a);
    assert_eq!(backend.shared.state.lock().unwrap().reserved, cost);
    {
        let _ambiguous = backend.shared.reserve(Some(Route::Translation), 0).unwrap();
    }
    assert!(!backend.enabled());
    assert!(backend.shared.reserve(Some(Route::Live), 0).is_err());
}
#[test]
fn audio_text_budgets_and_tts_exclusivity_are_independent() {
    let mut cfg = config();
    cfg.max_input_audio_seconds = 1;
    cfg.max_output_audio_seconds = 1;
    cfg.max_text_characters = 2;
    let backend = OpenAiBackend::new(cfg).unwrap();
    backend.shared.input(16000).unwrap();
    assert!(backend.shared.input(1).is_err());
    backend.shared.output(32000, 16000).unwrap();
    assert!(backend.shared.output(2, 24000).is_err());
    backend.shared.text("日本").unwrap();
    assert!(backend.shared.text("語").is_err());
    let backend = OpenAiBackend::new(config()).unwrap();
    let mut hold = backend.shared.reserve(None, 100).unwrap();
    assert!(backend.shared.reserve(None, 100).is_err());
    hold.completed = true;
    drop(hold);
    assert!(backend.shared.reserve(None, 100).is_ok());
}
#[test]
fn wav_has_exact_pcm_identity_and_duration_with_closed_bounds() {
    let pcm = vec![0u8; 48_000];
    let result = audio::wav(pcm.clone()).unwrap();
    assert_eq!(result.duration_ms, 1000);
    assert_eq!(result.sample_rate, 24000);
    assert_eq!(&result.wav[44..], pcm);
    assert_eq!(&result.wav[0..4], b"RIFF");
    assert_eq!(
        u32::from_le_bytes(result.wav[40..44].try_into().unwrap()),
        48000
    );
    assert!(audio::wav(Vec::new()).is_err());
    assert!(audio::wav(vec![0]).is_err());
    assert!(audio::wav(vec![0; audio::MAX_TTS_PCM + 2]).is_err());
}
#[test]
fn fir_conversion_preserves_rate_continuity_and_rejects_spectral_image() {
    fn amplitude(freq: f64) -> f64 {
        let mut filter = audio::Upsample::new();
        let mut out = Vec::new();
        for frame in 0..30 {
            let bytes: Vec<u8> = (0..320)
                .flat_map(|i| {
                    (((std::f64::consts::TAU * freq * (frame * 320 + i) as f64 / 16000.0).sin()
                        * 10000.0) as i16)
                        .to_le_bytes()
                })
                .collect();
            let b = filter.frame(&bytes).unwrap();
            assert_eq!(b.len(), 960);
            out.extend(
                b.chunks_exact(2)
                    .map(|p| i16::from_le_bytes([p[0], p[1]]) as f64),
            );
        }
        let samples = &out[480..];
        (samples.iter().map(|n| n * n).sum::<f64>() / samples.len() as f64).sqrt()
    }
    let pass = amplitude(1000.0);
    assert!((7000.0..7150.0).contains(&pass));
    let edge = amplitude(7900.0);
    assert!(
        edge < pass * 0.15,
        "anti-imaging cutoff must attenuate near Nyquist"
    );
    let mut a = audio::Upsample::new();
    assert!(a.frame(&[0; 638]).is_err());
    for _ in 0..5 {
        assert!(a.frame(&[0; 640]).unwrap().iter().all(|b| *b == 0));
    }
}
struct Noop;
impl Wake for Noop {
    fn wake(self: Arc<Self>) {}
}
#[tokio::test(flavor = "current_thread")]
async fn monotonic_deadline_rechecked_on_first_and_repoll_without_timer_driver() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let polled = AtomicUsize::new(0);
    assert!(
        transport::deadline(Instant::now(), async {
            polled.fetch_add(1, Ordering::Relaxed);
        })
        .await
        .is_err()
    );
    assert_eq!(polled.load(Ordering::Relaxed), 0);
    let future = std::future::poll_fn(|_| {
        polled.fetch_add(1, Ordering::Relaxed);
        Poll::<()>::Pending
    });
    let mut guarded = Box::pin(transport::deadline(
        Instant::now() + Duration::from_millis(30),
        future,
    ));
    let waker = Waker::from(Arc::new(Noop));
    let mut context = Context::from_waker(&waker);
    assert!(guarded.as_mut().poll(&mut context).is_pending());
    std::thread::sleep(Duration::from_millis(40));
    assert!(matches!(
        guarded.as_mut().poll(&mut context),
        Poll::Ready(Err(ProviderError::Deadline))
    ));
    assert_eq!(polled.load(Ordering::Relaxed), 1);
}
#[tokio::test]
async fn stop_prevents_even_immediately_ready_transport_poll() {
    let (tx, mut stop) = watch::channel(false);
    let (_global_tx, mut global) = watch::channel(false);
    tx.send_replace(true);
    let mut called = false;
    assert!(
        transport::active(
            Instant::now() + Duration::from_secs(1),
            &mut stop,
            &mut global,
            async {
                called = true;
            }
        )
        .await
        .is_err()
    );
    assert!(!called);
}

async fn pair(
    capacity: usize,
) -> (
    tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
    tokio_tungstenite::WebSocketStream<tokio::io::DuplexStream>,
) {
    let (a, b) = tokio::io::duplex(capacity);
    let client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        a,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let server = tokio_tungstenite::WebSocketStream::from_raw_socket(
        b,
        tokio_tungstenite::tungstenite::protocol::Role::Server,
        None,
    )
    .await;
    (client, server)
}
#[tokio::test]
async fn actual_in_memory_translation_handshake_output_and_stop_close() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (_input, input) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Translation, Language::Ja);
        let r = transport::drive(
            &shared,
            &mut client,
            &mut p,
            input,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert!(r.is_ok());
        transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await
    });
    server
        .send(Message::Text(
            json!({"type":"session.created","session":resolved(Route::Translation)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let request = server.next().await.unwrap().unwrap().into_text().unwrap();
    let request: Value = serde_json::from_str(&request).unwrap();
    assert_eq!(request, protocol::start(Route::Translation, Language::Ja));
    server
        .send(Message::Text(
            json!({"type":"session.updated","session":resolved(Route::Translation)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Ready {
            expires_in_seconds: 1..=20
        })
    ));
    server
        .send(Message::Text(
            json!({"type":"session.output_transcript.delta","delta":"日本語"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Transcript {
            provenance: Provenance::Translation,
            ..
        })
    ));
    stop.send_replace(true);
    let close: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(close["type"], "session.close");
    server
        .send(Message::Text(
            json!({"type":"session.closed","event_id":"closed"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(task.await.unwrap());
    assert!(output.try_recv().is_err());
}
#[tokio::test]
async fn full_consumer_queue_does_not_block_upstream_close() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (_input, input) = tokio::sync::mpsc::channel(8);
    let (events, _output) = tokio::sync::mpsc::channel(32);
    for _ in 0..32 {
        events
            .try_send(StreamEvent::Ready {
                expires_in_seconds: 1,
            })
            .unwrap_or_else(|_| panic!("fill"));
    }
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        let r = transport::drive(
            &shared,
            &mut client,
            &mut p,
            input,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert_eq!(r, Err((false, ProviderError::ProtocolCapacityLimit)));
        transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await
    });
    let _: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    server
        .send(Message::Text(
            json!({"type":"session.started","session":resolved(Route::Live)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let close: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(close["type"], "session.close");
    server.send(Message::Text(json!({"type":"session.closed","session":{"id":"sess_synthetic","model":protocol::LIVE},"reason":"close_requested","usage":{"seconds":1}}).to_string().into())).await.unwrap();
    assert!(task.await.unwrap());
}
#[tokio::test]
async fn cancelled_partial_websocket_write_is_dirty_and_not_flushed() {
    use tokio::io::AsyncReadExt;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (a, mut peer) = tokio::io::duplex(16);
    let mut client = tokio_tungstenite::WebSocketStream::from_raw_socket(
        a,
        tokio_tungstenite::tungstenite::protocol::Role::Client,
        None,
    )
    .await;
    let (_input, input) = tokio::sync::mpsc::channel(8);
    let (events, _output) = tokio::sync::mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        let r = transport::drive(
            &shared,
            &mut client,
            &mut p,
            input,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        drop(client);
        r
    });
    let mut first = [0; 1];
    peer.read_exact(&mut first).await.unwrap();
    stop.send_replace(true);
    assert_eq!(task.await.unwrap(), Err((true, ProviderError::Deadline)));
    let mut rest = Vec::new();
    peer.read_to_end(&mut rest).await.unwrap();
    assert!(
        rest.len() <= 16,
        "cancelled buffered application bytes must not be flushed"
    );
}
#[tokio::test]
async fn owned_backend_shutdown_joins_its_routes() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let mut stop = backend.shared.stop.subscribe();
    let task = tokio::spawn(async move {
        stop.changed().await.unwrap();
        assert!(*stop.borrow());
    });
    backend.shared.tasks.lock().unwrap().push(task);
    backend.shutdown().await;
    assert!(backend.shared.tasks.lock().unwrap().is_empty());
    assert!(!backend.enabled());
}
#[test]
fn explicit_route_allowlist_and_budget_capabilities_do_not_enable_other_models() {
    let mut cfg = config();
    cfg.allowed_routes = vec![Route::Live];
    cfg.allow_tts = false;
    let backend = OpenAiBackend::new(cfg).unwrap();
    assert!(backend.supports(Route::Live));
    assert!(!backend.supports(Route::Transcription));
    assert!(!backend.supports(Route::Translation));
    assert!(!backend.supports_tts());
    assert!(
        backend
            .shared
            .reserve(Some(Route::Transcription), 0)
            .is_err()
    );
    let mut cfg = config();
    cfg.approved_budget_microusd = 299; // ten UTF-8 bytes reserve 300 micro-USD
    let backend = OpenAiBackend::new(cfg).unwrap();
    assert!(backend.supports_tts()); // configured capability, not a spending permit
    assert!(!backend.budget_snapshot().tts.can_reserve_maximum);
    assert!(backend.shared.reserve(None, 10).is_err());
    let mut cfg = config();
    cfg.allowed_routes = vec![Route::Live, Route::Live];
    assert!(OpenAiBackend::new(cfg).is_err());
}
#[test]
fn live_reservation_bounds_usage_and_rejects_backend_delegation() {
    let mut p = ready(Route::Live);
    p.max_seconds = 20;
    assert!(
        p.consume(json!({"type":"session.usage.updated","usage":{"seconds":20.1}}))
            .is_err()
    );
    assert!(p.consume(json!({"type":"session.delegation.created","delegation":{"id":"del_a","target":"responses"}})).is_err());
    assert!(p.consume(json!({"type":"session.delegation.created","delegation":{"id":"del_a","target":"client"}})).unwrap().is_empty());
}
#[test]
fn fir_rejects_the_16khz_interpolation_image_after_24khz_conversion() {
    let mut filter = audio::Upsample::new();
    let mut result = Vec::new();
    for frame in 0..51 {
        let bytes: Vec<u8> = (0..320)
            .flat_map(|i| {
                (((std::f64::consts::TAU * 1000.0 * (frame * 320 + i) as f64 / 16000.0).sin()
                    * 12000.0) as i16)
                    .to_le_bytes()
            })
            .collect();
        result.extend(
            filter
                .frame(&bytes)
                .unwrap()
                .chunks_exact(2)
                .map(|b| i16::from_le_bytes([b[0], b[1]]) as f64),
        );
    }
    let samples = &result[480..];
    let amplitude = |hz: f64| {
        let (re, im) = samples
            .iter()
            .enumerate()
            .fold((0.0, 0.0), |(re, im), (i, s)| {
                let phase = std::f64::consts::TAU * hz * i as f64 / 24000.0;
                (re + s * phase.cos(), im + s * phase.sin())
            });
        (re * re + im * im).sqrt() / samples.len() as f64
    };
    assert!(amplitude(1000.0) > 5900.0);
    assert!(
        amplitude(9000.0) < amplitude(1000.0) * 0.001,
        "aliased15kHz image should be suppressed >60dB"
    );
}
#[tokio::test]
async fn actual_in_memory_asr_close_waits_for_each_final_usage_then_peer_handshake() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (mut client, mut server) = pair(4096).await;
    let mut p = ready(Route::Transcription);
    p.frames = 100;
    p.commit().unwrap();
    p.consume(json!({"type":"input_audio_buffer.committed","item_id":"item_a"}))
        .unwrap();
    let task = tokio::spawn(async move {
        transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await
    });
    server.send(Message::Text(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"item_a","content_index":0,"transcript":"synthetic","usage":{"type":"duration","seconds":2}}).to_string().into())).await.unwrap();
    assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
    let _ = server.flush().await;
    assert!(task.await.unwrap());
}
#[tokio::test]
async fn missing_terminal_confirmation_expires_and_poison_blocks_new_work() {
    use futures_util::StreamExt;
    let backend = OpenAiBackend::new(config()).unwrap();
    let (mut client, mut server) = pair(4096).await;
    let mut p = ready(Route::Live);
    let mut reservation = backend.shared.reserve(Some(Route::Live), 0).unwrap();
    let task = tokio::spawn(async move {
        transport::close(
            &mut client,
            &mut p,
            Instant::now() + Duration::from_millis(30),
        )
        .await
    });
    assert!(server.next().await.is_some());
    reservation.completed = task.await.unwrap();
    assert!(!reservation.completed);
    drop(reservation);
    assert!(!backend.enabled());
    assert!(!backend.supports(Route::Live));
}
#[test]
fn silence_completion_accounts_usage_emits_sealed_tombstone_and_next_turn_survives() {
    let mut p = ready(Route::Transcription);
    p.frames = 200;
    for id in ["silent", "speech"] {
        p.commit().unwrap();
        p.consume(json!({"type":"input_audio_buffer.committed","item_id":id}))
            .unwrap();
    }
    let result=p.consume(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"silent","content_index":0,"transcript":"","usage":{"type":"duration","seconds":2}})).unwrap();
    assert!(
        matches!(&result[..], [StreamEvent::Transcript{text,final_fragment:true,item:Some(item),..}] if text.is_empty() && item.id=="silent")
    );
    assert_eq!(p.usage, 2.0);
    let result=p.consume(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"speech","content_index":0,"transcript":"later speech","usage":{"type":"duration","seconds":2}})).unwrap();
    assert_eq!(result.len(), 1);
    assert!(p.complete_input());
    assert_eq!(p.usage, 4.0);
    let mut p = ready(Route::Live);
    assert!(
        p.consume(
            json!({"type":"session.output_transcript.delta","delta":"","start_ms":0,"end_ms":0})
        )
        .unwrap()
        .is_empty()
    );
    let mut p = ready(Route::Translation);
    assert!(
        p.consume(json!({"type":"session.output_transcript.delta","delta":""}))
            .unwrap()
            .is_empty()
    );
}
#[test]
fn pcm_event_bound_matches_browser_exactly() {
    let mut p = ready(Route::Live);
    assert!(
        p.consume(
            json!({"type":"session.output_audio.delta","delta":STANDARD.encode(vec![0;48000])})
        )
        .is_ok()
    );
    assert!(
        p.consume(
            json!({"type":"session.output_audio.delta","delta":STANDARD.encode(vec![0;48002])})
        )
        .is_err()
    );
}
#[test]
fn asr_declared_predecessor_cannot_silently_reorder_private_notes() {
    let mut p = ready(Route::Transcription);
    p.commit().unwrap();
    p.consume(
        json!({"type":"input_audio_buffer.committed","item_id":"first","previous_item_id":null}),
    )
    .unwrap();
    p.commit().unwrap();
    assert!(p.consume(json!({"type":"input_audio_buffer.committed","item_id":"second","previous_item_id":"unrelated"})).is_err());
}
#[test]
fn shutdown_serializes_late_task_registration_without_detached_work() {
    use std::sync::atomic::{AtomicUsize, Ordering};
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let backend = Arc::new(OpenAiBackend::new(config()).unwrap());
    let registry = backend.shared.tasks.lock().unwrap();
    let polled = Arc::new(AtomicUsize::new(0));
    let counter = polled.clone();
    let producer = backend.clone();
    let handle = runtime.handle().clone();
    let (entered, entry) = std::sync::mpsc::channel();
    let registration = std::thread::spawn(move || {
        let _guard = handle.enter();
        entered.send(()).unwrap();
        producer.shared.spawn_owned(async move {
            counter.fetch_add(1, Ordering::SeqCst);
        })
    });
    entry.recv_timeout(Duration::from_secs(1)).unwrap();
    let shutdown_backend = backend.clone();
    let handle = runtime.handle().clone();
    let shutdown = std::thread::spawn(move || handle.block_on(shutdown_backend.shutdown()));
    let deadline = Instant::now() + Duration::from_secs(1);
    while !*backend.shared.stop.borrow() {
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
    drop(registry);
    assert_eq!(registration.join().unwrap(), Err(ProviderError::Deadline));
    shutdown.join().unwrap();
    assert_eq!(polled.load(Ordering::SeqCst), 0);
    assert!(backend.shared.tasks.lock().unwrap().is_empty());
}

fn speech(p: &mut protocol::Protocol, id: &str, start: f64, end: f64, previous: Value) {
    p.consume(
        json!({"type":"input_audio_buffer.speech_started","item_id":id,"audio_start_ms":start}),
    )
    .unwrap();
    p.consume(json!({"type":"input_audio_buffer.speech_stopped","item_id":id,"audio_end_ms":end}))
        .unwrap();
    p.consume(
        json!({"type":"input_audio_buffer.committed","item_id":id,"previous_item_id":previous}),
    )
    .unwrap();
}
fn final_item(id: &str, text: &str) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":text,"usage":{"type":"duration","seconds":1.0}})
}
#[test]
fn server_vad_streams_one_utterance_past_two_seconds_then_seals_correction() {
    let mut p = ready(Route::Transcription);
    p.frames = 1;
    p.consume(json!({"type":"input_audio_buffer.speech_started","item_id":"spoken_a","audio_start_ms":0.25})).unwrap();
    for (revision, delta, expected) in [(1, "Asha and ", "Asha and "), (2, "Can", "Asha and Can")] {
        // Crossing 100 frames no longer commits or finalizes a turn.
        p.frames = 120 + revision;
        let out=p.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"spoken_a","content_index":0,"delta":delta})).unwrap();
        assert!(
            matches!(&out[..],[StreamEvent::Transcript{text,final_fragment:false,item:Some(item),..}] if text==expected && item.id=="spoken_a" && item.sequence==1 && item.revision==revision as u32)
        );
        assert!(!p.complete_input());
    }
    p.consume(json!({"type":"input_audio_buffer.speech_stopped","item_id":"spoken_a","audio_end_ms":2400.5})).unwrap();
    p.consume(
        json!({"type":"input_audio_buffer.committed","item_id":"spoken_a","previous_item_id":null}),
    )
    .unwrap();
    let out = p.consume(final_item("spoken_a", "Asha and Ken")).unwrap();
    assert!(
        matches!(&out[..],[StreamEvent::Transcript{text,final_fragment:true,item:Some(item),..}] if text=="Asha and Ken" && item.revision==3 && item.sequence==1)
    );
    assert!(p.complete_input());
    assert!(p.consume(final_item("spoken_a", "resurrection")).is_err());
}
#[test]
fn vad_empty_final_seals_partial_and_out_of_order_finals_preserve_item_order() {
    let mut p = ready(Route::Transcription);
    p.frames = 250;
    speech(&mut p, "first", 0.0, 1000.0, Value::Null);
    let out=p.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"first","content_index":0,"delta":"uncertain"})).unwrap();
    assert!(matches!(
        &out[..],
        [StreamEvent::Transcript {
            final_fragment: false,
            ..
        }]
    ));
    speech(&mut p, "second", 1100.0, 2000.0, json!("first"));
    assert!(
        p.consume(final_item("second", "later source"))
            .unwrap()
            .is_empty()
    );
    let out = p.consume(final_item("first", "")).unwrap();
    assert!(
        matches!(&out[..],[StreamEvent::Transcript{text,final_fragment:true,item:Some(a),..},StreamEvent::Transcript{item:Some(b),..}] if text.is_empty() && a.sequence==1 && a.revision==2 && b.sequence==2)
    );
    assert!(p.complete_input());
    assert!(p.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"first","content_index":0,"delta":"late"})).is_err());
}
#[test]
fn vad_rejects_unknown_commit_wrong_predecessor_oversize_and_long_turn() {
    let mut p = ready(Route::Transcription);
    p.frames = 100;
    assert!(
        p.consume(json!({"type":"input_audio_buffer.committed","item_id":"unknown"}))
            .is_err()
    );
    for previous in [json!(10), json!([]), json!({}), json!("unrelated")] {
        let mut p = ready(Route::Transcription);
        p.frames = 100;
        p.consume(
            json!({"type":"input_audio_buffer.speech_started","item_id":"a","audio_start_ms":0}),
        )
        .unwrap();
        p.consume(
            json!({"type":"input_audio_buffer.speech_stopped","item_id":"a","audio_end_ms":1000}),
        )
        .unwrap();
        assert!(p.consume(json!({"type":"input_audio_buffer.committed","item_id":"a","previous_item_id":previous})).is_err());
    }
    let mut p = ready(Route::Transcription);
    p.frames = 1;
    p.consume(
        json!({"type":"input_audio_buffer.speech_started","item_id":"long","audio_start_ms":0}),
    )
    .unwrap();
    p.frames = 5999;
    assert!(p.asr.check(p.frames).is_ok());
    p.frames = 6000;
    assert_eq!(
        p.asr.check(p.frames),
        Err(ProviderError::SpeechBoundaryLimit)
    );
    let mut p = ready(Route::Transcription);
    p.frames = 1;
    p.consume(
        json!({"type":"input_audio_buffer.speech_started","item_id":"oversize","audio_start_ms":0}),
    )
    .unwrap();
    assert!(p.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"oversize","content_index":0,"delta":"あ".repeat(4097)})).is_err());
}
#[test]
fn upstream_error_categories_are_exact_static_and_never_copy_sensitive_fields() {
    for (kind, code, expected) in [
        (
            "invalid_request_error",
            "invalid_event",
            DiagnosticCategory::UpstreamInvalidEvent,
        ),
        (
            "server_error",
            "unseen",
            DiagnosticCategory::UpstreamServerError,
        ),
        (
            "transcription_error",
            "audio_unintelligible",
            DiagnosticCategory::UpstreamAudioUnintelligible,
        ),
        (
            "transcription_error",
            "unseen",
            DiagnosticCategory::UpstreamTranscription,
        ),
        (
            "invalid_request_error",
            "unseen",
            DiagnosticCategory::UpstreamInvalidRequest,
        ),
        (
            "unknown_untrusted_type",
            "unknown",
            DiagnosticCategory::UpstreamError,
        ),
    ] {
        let canary = "synthetic-secret-message-never-retain";
        let value = json!({"type":"error","error":{"type":kind,"code":code,"message":canary,"param":canary}});
        let (event, category) = diagnostics::rejected_event(Route::Live, true, &value);
        assert_eq!(category, expected);
        let encoded = serde_json::to_string(&(event, category)).unwrap();
        assert!(!encoded.contains(canary));
        assert!(!encoded.contains("unknown_untrusted_type"));
    }
}

#[tokio::test]
async fn finish_input_observes_late_output_past_old_five_second_cutoff_without_more_audio() {
    use crate::engine::{DrainCompletion, StreamInput};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let start = Instant::now();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Translation, Language::Ja);
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            start + Duration::from_millis(15800),
        )
        .await
        .unwrap();
        assert_eq!(
            p.frames, 0,
            "FinishInput sends no fabricated input or audio commit"
        );
        transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await
    });
    server
        .send(Message::Text(
            json!({"type":"session.created","session":resolved(Route::Translation)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let startup: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(startup, protocol::start(Route::Translation, Language::Ja));
    server
        .send(Message::Text(
            json!({"type":"session.updated","session":resolved(Route::Translation)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Ready { .. })
    ));
    input.send(StreamInput::FinishInput).await.unwrap();
    tokio::time::sleep(Duration::from_millis(5150)).await;
    server
        .send(Message::Text(
            json!({"type":"session.output_transcript.delta","delta":"late final words"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await,Some(StreamEvent::Transcript{text,..}) if text=="late final words")
    );
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::ObservationWindowEnded
        })
    ));
    assert!(start.elapsed() >= Duration::from_millis(5700));
    let close: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(close["type"], "session.close");
    server
        .send(Message::Text(
            json!({"type":"session.closed","event_id":"closed"})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(
        task.await.unwrap(),
        "translation requires exact terminal session.closed receipt"
    );
}
#[tokio::test]
async fn stop_during_finish_input_suppresses_late_transcript_and_drained_event() {
    use crate::engine::StreamInput;
    use futures_util::SinkExt;
    use tokio_tungstenite::tungstenite::Message;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = ready(Route::Translation);
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await
    });
    input.send(StreamInput::FinishInput).await.unwrap();
    tokio::time::sleep(Duration::from_millis(25)).await;
    stop.send_replace(true);
    let _ = server
        .send(Message::Text(
            json!({"type":"session.output_transcript.delta","delta":"must suppress"})
                .to_string()
                .into(),
        ))
        .await;
    assert!(task.await.unwrap().is_ok());
    assert!(output.recv().await.is_none());
}

#[tokio::test]
async fn real_local_vad_transport_keeps_speech_beyond_two_seconds_until_explicit_finish() {
    use crate::engine::{DrainCompletion, StreamInput};
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let pcm = crate::test_audio::signal(150, 0);
    let frames = pcm
        .chunks_exact(640)
        .take(150)
        .map(<[u8]>::to_vec)
        .collect::<Vec<_>>();
    assert_eq!(frames.len(), 150);
    let mut witness = local_vad::LocalVad::new().unwrap();
    let mut expected_frames = 0;
    for frame in &frames {
        let segment = witness.push(frame).unwrap();
        assert!(
            segment.commit.is_none(),
            "continuous synthetic speech has no natural boundary yet"
        );
        expected_frames += segment.append.len() / 640;
    }
    assert!(expected_frames > 100 && witness.active());
    assert_eq!(
        witness.finish().unwrap().commit.unwrap().reason,
        local_vad::CommitReason::FinishInput
    );
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(p.captured_frames, 150);
        assert_eq!(p.frames, expected_frames as u64);
        assert!(p.complete_input());
        transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await
    });
    server
        .send(Message::Text(
            json!({"type":"session.created","session":resolved(Route::Transcription)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let start: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert!(
        start["session"]["audio"]["input"]
            .as_object()
            .unwrap()
            .contains_key("turn_detection")
    );
    assert!(start["session"]["audio"]["input"]["turn_detection"].is_null());
    server
        .send(Message::Text(
            json!({"type":"session.updated","session":resolved(Route::Transcription)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Ready { .. })
    ));
    let feeder = tokio::spawn(async move {
        for frame in frames {
            input.send(StreamInput::Pcm16(frame)).await.unwrap();
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
        input.send(StreamInput::FinishInput).await.unwrap();
        input
    });
    let mut sent = 0;
    let mut early = false;
    loop {
        let event: Value =
            serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap())
                .unwrap();
        if event["type"] == "input_audio_buffer.commit" {
            assert_eq!(event["event_id"], "pilot_commit_1_finish");
            assert_eq!(sent, expected_frames, "no periodic two-second commit");
            break;
        }
        assert_eq!(event["type"], "input_audio_buffer.append");
        sent += STANDARD
            .decode(event["audio"].as_str().unwrap())
            .unwrap()
            .len()
            / 960;
        if sent > 100 && !early {
            early = true;
            let received_at = Instant::now();
            server.send(Message::Text(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"continuous","content_index":0,"delta":"still one utterance"}).to_string().into())).await.unwrap();
            assert!(
                matches!(tokio::time::timeout(Duration::from_millis(500),output.recv()).await.unwrap(),
                Some(StreamEvent::SourceDraft{item_id,revision:1,text,retired:false}) if item_id=="continuous" && text=="still one utterance")
            );
            assert!(
                expected_frames > sent + 25,
                "draft arrives before at least500ms of remaining selected source, without a commit"
            );
            println!(
                "manual ASR synthetic transport: draft forwarded in {:?}, at selected frame {sent}; final commit later at {expected_frames}; provider latency and recognition quality not measured",
                received_at.elapsed()
            );
            server.send(Message::Text(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"continuous","content_index":0,"delta":" with a number: 12,000"}).to_string().into())).await.unwrap();
            assert!(
                matches!(tokio::time::timeout(Duration::from_millis(500),output.recv()).await.unwrap(),
                Some(StreamEvent::SourceDraft{revision:2,text,retired:false,..}) if text=="still one utterance with a number: 12,000")
            );
        }
    }
    assert!(early);
    assert!(
        output.try_recv().is_err(),
        "no canonical source item exists before its receipt"
    );
    let _input = feeder.await.unwrap();
    server.send(Message::Text(json!({"type":"input_audio_buffer.committed","item_id":"continuous","previous_item_id":null}).to_string().into())).await.unwrap();
    assert!(
        matches!(output.recv().await,Some(StreamEvent::SourceDraft{item_id,revision:2,text,retired:true}) if item_id=="continuous" && text.is_empty())
    );
    assert!(
        matches!(output.recv().await,Some(StreamEvent::Transcript{final_fragment:false,item:Some(item),..}) if item.id=="continuous" && item.revision==1)
    );
    server
        .send(Message::Text(
            final_item("continuous", "one complete utterance")
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await,Some(StreamEvent::Transcript{final_fragment:true,item:Some(item),..}) if item.revision==2)
    );
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
    server.flush().await.unwrap();
    assert!(task.await.unwrap());
}
#[test]
fn server_vad_pending_capacity_and_bad_timestamps_fail_closed() {
    let mut p = ready(Route::Transcription);
    p.frames = 500;
    for index in 0..8 {
        speech(
            &mut p,
            &format!("item_{index}"),
            index as f64 * 1000.0,
            index as f64 * 1000.0 + 500.0,
            if index == 0 {
                Value::Null
            } else {
                json!(format!("item_{}", index - 1))
            },
        );
    }
    assert!(p.consume(json!({"type":"input_audio_buffer.speech_started","item_id":"ninth","audio_start_ms":8000})).is_err());
    for timestamp in [Value::Null, json!(-0.1), json!(100000), json!("10")] {
        let mut p = ready(Route::Transcription);
        p.frames = 1;
        assert!(p.consume(json!({"type":"input_audio_buffer.speech_started","item_id":"bad_time","audio_start_ms":timestamp})).is_err());
    }
}

#[test]
fn prepared_tts_uses_fixed_character_priced_model_and_exact_input_without_instructions() {
    for input in [
        "Read this exactly.",
        "日本語の原稿です。",
        "यह काल्पनिक परीक्षण है।",
        "😀e\u{301}",
    ] {
        let body = protocol::tts_request(input).unwrap();
        assert_eq!(
            body,
            json!({"model":"tts-1-hd","voice":"alloy","input":input,"response_format":"pcm","speed":1.0})
        );
        assert!(body.get("instructions").is_none());
        assert!(protocol::tts_cost(input.len()).unwrap() >= input.chars().count() as u64 * 30);
    }
    assert_eq!(protocol::tts_cost(2000).unwrap(), 60_000);
    for text in ["", " \n", "contains\0nul"] {
        assert!(protocol::tts_request(text).is_err());
    }
    assert!(protocol::tts_request(&"x".repeat(2001)).is_err());
    assert!(protocol::tts_cost(2001).is_err());
    assert!(protocol::tts_request(&"😀".repeat(500)).is_ok());
    assert!(protocol::tts_request(&"😀".repeat(501)).is_err());
}
#[test]
fn tts_character_reservation_is_unrefunded_and_blocks_next_request_before_network() {
    let mut cfg = config();
    cfg.approved_budget_microusd = 61_000;
    let backend = OpenAiBackend::new(cfg).unwrap();
    assert!(backend.supports_tts());
    let mut first = backend.shared.reserve(None, 2000).unwrap();
    assert_eq!(backend.shared.state.lock().unwrap().reserved, 60_000);
    first.completed = true;
    drop(first);
    assert!(
        !backend.budget_snapshot().tts.can_reserve_maximum,
        "maximum quote cannot fit, while exact smaller requests remain possible"
    );
    assert!(backend.supports_tts());
    assert!(backend.shared.reserve(None, 34).is_err());
    assert_eq!(backend.shared.state.lock().unwrap().reserved, 60_000);
    let mut short = backend.shared.reserve(None, 33).unwrap();
    short.completed = true;
    drop(short);
    assert_eq!(backend.shared.state.lock().unwrap().reserved, 60_990);
}

#[test]
fn error_details_are_exact_allowlists_and_first_failure_is_preserved() {
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    let private = "synthetic-private-canary-never-retain";
    assert!(
        p.consume(json!({"type":"error","event_id":private,"error":{
            "type":"invalid_request_error","code":"unsupported_value",
            "param":"session.audio.input.turn_detection","message":private
        }}))
        .is_err()
    );
    assert_eq!(
        p.failure_details,
        (
            DiagnosticCode::UnsupportedValue,
            DiagnosticParameter::TurnDetection
        )
    );
    assert!(p.consume(json!({"type":"error","error":{"code":"invalid_value","param":"session.audio.input.transcription.delay"}})).is_err());
    assert_eq!(
        p.failure_details,
        (
            DiagnosticCode::UnsupportedValue,
            DiagnosticParameter::TurnDetection
        )
    );
    for value in [
        json!(private),
        json!({"private":private}),
        json!([private]),
        json!(true),
        json!(3),
        json!("x".repeat(65536)),
    ] {
        let details = diagnostics::upstream_details(
            &json!({"type":"error","error":{"code":value,"param":value,"message":private}}),
        );
        assert_eq!(details, (DiagnosticCode::Other, DiagnosticParameter::Other));
        assert!(!serde_json::to_string(&details).unwrap().contains(private));
    }
    for value in [
        json!({"type":"error"}),
        json!({"type":"error","error":{"code":null,"param":null}}),
        json!({"type":"private","error":{"code":"invalid_value","param":"session.type"}}),
    ] {
        assert_eq!(diagnostics::upstream_details(&value), Default::default());
    }
    assert_eq!(
        diagnostics::upstream_details(
            &json!({"type":"error","error":{"code":"unknown_parameter","param":"session.audio.input.turn_detection.silence_duration_ms"}})
        ),
        (
            DiagnosticCode::UnknownParameter,
            DiagnosticParameter::TurnDetectionSilenceDuration
        )
    );
}

#[tokio::test]
async fn rejected_asr_startup_without_audio_still_closes_peer_and_retains_failure() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (_input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        let result = transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert_eq!(result, Err((false, crate::engine::ProviderError::Protocol)));
        assert!(p.created);
        assert!(!p.ready);
        assert_eq!(p.frames, 0);
        assert!(p.complete_input());
        let original = p.failure;
        let confirmed =
            transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await;
        assert_eq!(p.failure, original);
        assert!(!p.ready);
        assert_eq!(
            p.failure_details,
            (
                DiagnosticCode::UnsupportedValue,
                DiagnosticParameter::TurnDetection
            )
        );
        confirmed
    });
    server
        .send(Message::Text(
            json!({"type":"session.created","session":resolved(Route::Transcription)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let startup: Value =
        serde_json::from_str(&server.next().await.unwrap().unwrap().into_text().unwrap()).unwrap();
    assert_eq!(startup, protocol::start(Route::Transcription, Language::Ja));
    server.send(Message::Text(json!({"type":"error","error":{"type":"invalid_request_error","code":"unsupported_value","param":"session.audio.input.turn_detection","message":"synthetic-only"}}).to_string().into())).await.unwrap();
    assert!(
        matches!(server.next().await, Some(Ok(Message::Close(_)))),
        "rejected startup still sends its normal transport close"
    );
    let _ = server.flush().await;
    assert!(task.await.unwrap());
    assert!(
        output.try_recv().is_err(),
        "startup rejection never grants Ready or emits content"
    );
}

#[tokio::test]
async fn rejected_asr_cleanup_requires_known_session_and_peer_confirmation() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    // A peer handshake alone cannot validate an unknown application session.
    let (mut client, mut server) = pair(4096).await;
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    let task = tokio::spawn(async move {
        transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await
    });
    assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
    let _ = server.flush().await;
    assert!(!task.await.unwrap());

    // Valid creation and zero input still need a peer close acknowledgement.
    let (mut client, _server) = pair(4096).await;
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    p.consume(json!({"type":"session.created","session":resolved(Route::Transcription)}))
        .unwrap();
    assert!(
        !transport::close(
            &mut client,
            &mut p,
            Instant::now() + Duration::from_millis(20)
        )
        .await
    );
    assert!(!p.ready);
    assert_eq!(p.frames, 0);
}

#[test]
fn retryable_transport_classification_preserves_auth_tls_and_schema_refusals() {
    use tokio_tungstenite::tungstenite::{Error, error::ProtocolError, http::Response};
    for status in [400, 401, 403, 404, 429] {
        let error = Error::Http(Box::new(
            Response::builder().status(status).body(None).unwrap(),
        ));
        assert_eq!(
            transport::transport_error(&error),
            ProviderError::Unavailable
        );
        assert!(!transport::transport_error(&error).recoverable());
    }
    for kind in [
        std::io::ErrorKind::InvalidData,
        std::io::ErrorKind::InvalidInput,
        std::io::ErrorKind::PermissionDenied,
    ] {
        assert_eq!(
            transport::transport_error(&Error::Io(std::io::Error::from(kind))),
            ProviderError::Unavailable
        );
    }
    for status in [500, 502, 503, 504] {
        let error = Error::Http(Box::new(
            Response::builder().status(status).body(None).unwrap(),
        ));
        assert_eq!(
            transport::transport_error(&error),
            ProviderError::ConnectionLost
        );
    }
    for error in [
        Error::ConnectionClosed,
        Error::AlreadyClosed,
        Error::Io(std::io::Error::from(std::io::ErrorKind::ConnectionReset)),
        Error::Protocol(ProtocolError::ResetWithoutClosingHandshake),
    ] {
        assert_eq!(
            transport::transport_error(&error),
            ProviderError::ConnectionLost
        );
    }
    assert_eq!(
        transport::transport_error(&Error::Protocol(ProtocolError::SendAfterClosing)),
        ProviderError::Unavailable
    );
}
