use super::*;
use crate::budget::ProviderVendor;
use crate::engine::Provenance;
use crate::provider::{FiniteBackend, ProviderConfig};
use tokio::io::{DuplexStream, duplex};
use tokio_tungstenite::tungstenite::protocol::Role;

fn backend() -> FiniteBackend {
    FiniteBackend::for_vendor(
        ProviderConfig {
            api_key: "fixture-only-not-transmitted".into(),
            allowed_routes: vec![Route::Transcription, Route::Live],
            allowed_live_modes: vec![LiveMode::Interpretation],
            allow_tts: false,
            total_duration_seconds: 100,
            route_duration_seconds: 60,
            max_sessions: 4,
            max_tts_requests: 0,
            max_input_audio_seconds: 60,
            max_output_audio_seconds: 60,
            max_text_characters: 4096,
            approved_budget_microusd: 5_000_000,
        },
        ProviderVendor::Gemini,
    )
    .unwrap()
}

fn protocol(route: Route) -> Protocol {
    Protocol::new(
        route,
        Language::En,
        (route == Route::Live).then_some(LiveMode::Interpretation),
        None,
        None,
        None,
    )
    .unwrap()
}

#[test]
fn source_language_selections_use_transcription_locales_and_targets_use_translation_codes() {
    use Language::{En, Es, Hi, Ja, Vi};
    let setup = |languages| {
        Protocol::new(
            Route::Transcription,
            En,
            None,
            Some(SpeechHints {
                languages,
                keywords: Vec::new(),
            }),
            None,
            None,
        )
    };
    for (languages, expected) in [
        (vec![En], json!(["en-US"])),
        (vec![Ja], json!(["ja-JP"])),
        (vec![Hi], json!(["hi-IN"])),
        (vec![Hi, Ja], json!(["hi-IN", "ja-JP"])),
    ] {
        assert_eq!(
            setup(languages).unwrap().setup["setup"]["inputAudioTranscription"]["languageCodes"],
            expected
        );
    }
    assert_eq!(
        protocol(Route::Transcription).setup["setup"]["inputAudioTranscription"]["languageCodes"],
        json!(["ja-JP", "en-US", "hi-IN"])
    );
    for unsupported in [Es, Vi] {
        assert!(matches!(
            setup(vec![En, unsupported]),
            Err(ProviderError::Unavailable)
        ));
    }
    for (language, target) in [(En, "en"), (Ja, "ja"), (Hi, "hi")] {
        let live = Protocol::new(
            Route::Live,
            language,
            Some(LiveMode::Interpretation),
            None,
            None,
            None,
        )
        .unwrap();
        assert_eq!(
            live.setup["setup"]["generationConfig"]["translationConfig"]["targetLanguageCode"],
            target
        );
    }
}
#[test]
fn handshake_and_setup_pin_vendor_model_route_and_authority() {
    let request = handshake("fixture-key").unwrap();
    assert_eq!(request.uri().scheme_str(), Some("wss"));
    assert_eq!(
        request.uri().host(),
        Some("generativelanguage.googleapis.com")
    );
    assert!(request.uri().query().is_none());
    assert!(!request.headers().contains_key("authorization"));
    let header = &request.headers()["x-goog-api-key"];
    assert!(header.is_sensitive());
    assert_eq!(header, "fixture-key");
    assert!(handshake("sk-test-fixture-only").is_err());
    let tx = protocol(Route::Transcription);
    assert_eq!(
        tx.setup["setup"]["model"],
        "models/gemini-3.5-transcribe-live"
    );
    assert_eq!(
        tx.setup["setup"]["inputAudioTranscription"]["mode"],
        "VERBATIM"
    );
    let interpreter = protocol(Route::Live);
    assert_eq!(
        interpreter.setup["setup"]["model"],
        "models/gemini-3.5-live-translate-preview"
    );
    assert_eq!(
        interpreter.setup["setup"]["generationConfig"]["translationConfig"]["targetLanguageCode"],
        "en"
    );
    assert!(!interpreter.setup.to_string().contains("tools"));
    for (route, mode) in [
        (Route::Translation, None),
        (Route::Live, None),
        (Route::Live, Some(LiveMode::Assistant)),
    ] {
        assert!(Protocol::new(route, Language::En, mode, None, None, None).is_err());
    }
}

#[path = "stop_receipt_tests.rs"]
mod stop_receipt_tests;

async fn read_json(socket: &mut WebSocketStream<DuplexStream>) -> Value {
    let msg = tokio::time::timeout(Duration::from_secs(2), socket.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    serde_json::from_str(msg.to_text().unwrap()).unwrap()
}
async fn send(socket: &mut WebSocketStream<DuplexStream>, value: Value) {
    socket
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}
async fn send_binary_json(socket: &mut WebSocketStream<DuplexStream>, value: Value) {
    socket
        .send(Message::Binary(serde_json::to_vec(&value).unwrap().into()))
        .await
        .unwrap();
}
pub(super) async fn peer_pair() -> (WebSocketStream<DuplexStream>, WebSocketStream<DuplexStream>) {
    let (a, b) = duplex(4096);
    tokio::join!(
        WebSocketStream::from_raw_socket(a, Role::Client, None),
        WebSocketStream::from_raw_socket(b, Role::Server, None)
    )
}

#[tokio::test]
async fn both_routes_stream_from_setup_through_pcm_and_confirmed_stop() {
    let backend = backend();
    for route in [Route::Transcription, Route::Live] {
        let (mut local, mut peer) = peer_pair().await;
        let (input, rx) = mpsc::channel(8);
        let (tx, mut events) = mpsc::channel(32);
        let (stop, stopped) = watch::channel(false);
        let mut p = protocol(route);
        let mut scope = Scope {
            shared: &backend.shared,
            stop: stopped,
            global: backend.shared.stop.subscribe(),
            hard: Instant::now() + Duration::from_secs(10),
        };
        let service = drive(&mut scope, &mut local, &mut p, rx, &tx);
        let fixture = async {
            let setup = read_json(&mut peer).await;
            assert!(
                setup["setup"]["model"]
                    .as_str()
                    .unwrap()
                    .contains(if route == Route::Live {
                        "translate"
                    } else {
                        "transcribe"
                    })
            );
            send_binary_json(&mut peer, json!({"setupComplete":{}})).await;
            assert!(matches!(
                events.recv().await,
                Some(StreamEvent::Ready { .. })
            ));
            for _ in 0..5 {
                input.send(StreamInput::Pcm16(vec![31; 640])).await.unwrap();
            }
            let frame = read_json(&mut peer).await;
            assert_eq!(
                frame["realtimeInput"]["audio"]["mimeType"],
                "audio/pcm;rate=16000"
            );
            assert_eq!(
                STANDARD
                    .decode(frame["realtimeInput"]["audio"]["data"].as_str().unwrap())
                    .unwrap(),
                vec![31; 3200]
            );
            if route == Route::Transcription {
                send(
                    &mut peer,
                    json!({"serverContent":{"interimInputTranscription":{"text":"Forty"}}}),
                )
                .await;
                assert!(matches!(
                    events.recv().await,
                    Some(StreamEvent::SourceDraft { retired: false, .. })
                ));
                send_binary_json(
                    &mut peer,
                    json!({"serverContent":{"inputTranscription":{"text":"Fourteen."}}}),
                )
                .await;
                assert!(matches!(
                    events.recv().await,
                    Some(StreamEvent::SourceDraft { retired: true, .. })
                ));
                assert!(
                    matches!(events.recv().await,Some(StreamEvent::Transcript{provenance:Provenance::SourceAsr,final_fragment:true,text,item:Some(item)}) if text=="Fourteen." && item.sequence==1 && item.source_audio.is_none())
                );
            } else {
                send_binary_json(&mut peer,json!({"serverContent":{"outputTranscription":{"text":"Fourteen."},"modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=24000","data":"AQACAA=="}}]}}})).await;
                assert!(
                    matches!(events.recv().await,Some(StreamEvent::Transcript{provenance:Provenance::LiveOutput,text,item:None,..}) if text=="Fourteen.")
                );
                assert!(
                    matches!(events.recv().await,Some(StreamEvent::Audio{pcm16,sample_rate:24000}) if pcm16==[1,0,2,0])
                );
            }
            input.send(StreamInput::Pcm16(vec![8; 640])).await.unwrap();
            tokio::task::yield_now().await;
            stop.send_replace(true);
            // Audio submitted after the stop fence never crosses the wire.
            let _ = input.try_send(StreamInput::Pcm16(vec![9; 640]));
            assert!(matches!(peer.next().await, Some(Ok(Message::Close(_)))));
            peer.flush().await.unwrap();
        };
        let ((result, confirmed), ()) = tokio::join!(service, fixture);
        assert!(result.is_ok() && confirmed);
    }
}

#[tokio::test]
async fn malformed_or_oversized_binary_payload_is_rejected_at_setup_and_while_streaming() {
    let backend = backend();
    for started in [false, true] {
        let mut oversized = if started {
            br#"{"usageMetadata":{}}"#.to_vec()
        } else {
            br#"{"setupComplete":{}}"#.to_vec()
        };
        oversized.resize(65_537, b' ');
        for invalid in [vec![0xff], vec![1, 0, 2, 0], b"null".to_vec(), oversized] {
            let (mut local, mut peer) = peer_pair().await;
            let (_input, rx) = mpsc::channel(8);
            let (tx, mut events) = mpsc::channel(32);
            let (_stop, stopped) = watch::channel(false);
            let mut p = protocol(Route::Live);
            let mut scope = Scope {
                shared: &backend.shared,
                stop: stopped,
                global: backend.shared.stop.subscribe(),
                hard: Instant::now() + Duration::from_secs(5),
            };
            let service = drive(&mut scope, &mut local, &mut p, rx, &tx);
            let fixture = async {
                read_json(&mut peer).await;
                if started {
                    send_binary_json(&mut peer, json!({"setupComplete":{}})).await;
                    assert!(matches!(
                        events.recv().await,
                        Some(StreamEvent::Ready { .. })
                    ));
                }
                peer.send(Message::Binary(invalid.into())).await.unwrap();
                assert!(matches!(peer.next().await, Some(Ok(Message::Close(_)))));
                peer.flush().await.unwrap();
            };
            let ((result, confirmed), ()) = tokio::join!(service, fixture);
            assert_eq!(result, Err(ProviderError::Protocol));
            assert!(confirmed && events.try_recv().is_err());
        }
    }
}

#[tokio::test]
async fn provider_reset_and_bad_audio_never_confirm_closure_or_publish_output() {
    let backend = backend();
    let (mut local, mut peer) = peer_pair().await;
    let (input, rx) = mpsc::channel(8);
    let (tx, mut events) = mpsc::channel(32);
    let (_stop, stopped) = watch::channel(false);
    let mut p = protocol(Route::Live);
    let mut scope = Scope {
        shared: &backend.shared,
        stop: stopped,
        global: backend.shared.stop.subscribe(),
        hard: Instant::now() + Duration::from_secs(10),
    };
    let service = drive(&mut scope, &mut local, &mut p, rx, &tx);
    let fixture = async move {
        read_json(&mut peer).await;
        send(&mut peer, json!({"setupComplete":{}})).await;
        assert!(matches!(
            events.recv().await,
            Some(StreamEvent::Ready { .. })
        ));
        for _ in 0..5 {
            input.send(StreamInput::Pcm16(vec![0; 640])).await.unwrap();
        }
        read_json(&mut peer).await;
        send(&mut peer,json!({"serverContent":{"modelTurn":{"parts":[{"inlineData":{"mimeType":"audio/pcm;rate=16000","data":"AAAA"}}]}}})).await;
        drop(peer); // no peer close frame
    };
    let ((result, confirmed), ()) = tokio::join!(service, fixture);
    assert_eq!(result, Err(ProviderError::Protocol));
    assert!(!confirmed);
}

#[test]
fn interim_rewrite_source_provenance_and_pre_input_fences() {
    let backend = backend();
    let mut asr = protocol(Route::Transcription);
    asr.consume(&backend.shared, json!({"setupComplete":{}}))
        .unwrap();
    assert!(
        asr.consume(
            &backend.shared,
            json!({"serverContent":{"inputTranscription":{"text":"unsolicited"}}})
        )
        .is_err()
    );
    asr.frames = 1;
    for word in ["Friday", "Thursday"] {
        let row = asr
            .consume(
                &backend.shared,
                json!({"serverContent":{"interimInputTranscription":{"text":word}}}),
            )
            .unwrap();
        assert!(
            matches!(row.last(),Some(StreamEvent::SourceDraft{text,retired:false,..}) if text==word)
        );
    }
    assert!(
        asr.consume(
            &backend.shared,
            json!({"serverContent":{"outputTranscription":{"text":"do not export"}}})
        )
        .is_err()
    );
    assert!(
        asr.consume(&backend.shared, json!({"toolCall":{}}))
            .is_err()
    );
}

#[tokio::test]
async fn finish_flushes_short_tail_and_reports_observation_only() {
    let backend = backend();
    let (mut local, mut peer) = peer_pair().await;
    let (input, rx) = mpsc::channel(8);
    let (tx, mut events) = mpsc::channel(32);
    let (_stop, stopped) = watch::channel(false);
    let mut p = protocol(Route::Transcription);
    let mut scope = Scope {
        shared: &backend.shared,
        stop: stopped,
        global: backend.shared.stop.subscribe(),
        hard: Instant::now() + Duration::from_secs(8),
    };
    let service = drive(&mut scope, &mut local, &mut p, rx, &tx);
    let fixture = async {
        read_json(&mut peer).await;
        send(&mut peer, json!({"setupComplete":{}})).await;
        assert!(matches!(
            events.recv().await,
            Some(StreamEvent::Ready { .. })
        ));
        input.send(StreamInput::Pcm16(vec![2; 640])).await.unwrap();
        input.send(StreamInput::FinishInput).await.unwrap();
        assert_eq!(
            read_json(&mut peer).await["realtimeInput"]["audio"]["data"],
            STANDARD.encode(vec![2; 640])
        );
        assert_eq!(
            read_json(&mut peer).await,
            json!({"realtimeInput":{"audioStreamEnd":true}})
        );
        send(
            &mut peer,
            json!({"serverContent":{"turnComplete":true},"usageMetadata":{"totalTokenCount":10}}),
        )
        .await;
        assert!(matches!(
            events.recv().await,
            Some(StreamEvent::Drained {
                completion: DrainCompletion::ObservationWindowEnded
            })
        ));
        assert!(matches!(peer.next().await, Some(Ok(Message::Close(_)))));
        peer.flush().await.unwrap();
    };
    let ((result, confirmed), ()) = tokio::join!(service, fixture);
    assert!(result.is_ok() && confirmed);
}

#[tokio::test]
async fn pre_revoked_route_never_polls_a_secret_bearing_handshake() {
    let backend = backend();
    let (signal, stopped) = watch::channel(false);
    signal.send_replace(true);
    let deadline = Instant::now() + Duration::from_secs(2);
    let mut scope = Scope {
        shared: &backend.shared,
        stop: stopped,
        global: backend.shared.stop.subscribe(),
        hard: deadline,
    };
    let mut polled = false;
    let result = scope
        .guarded(deadline, true, async {
            polled = true;
        })
        .await;
    assert_eq!(result, Err(ProviderError::Deadline));
    assert!(!polled);
}
