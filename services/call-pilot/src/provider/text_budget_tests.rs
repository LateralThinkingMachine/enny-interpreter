//! Key-free text ingress accounting. Protocol traces represent bounded synthetic
//! durations; they are not a real-time soak, audio fixture or language benchmark.
use super::*;
use crate::engine::{LiveMode, StreamInput};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

fn pair_config(text_limit: u64) -> ProviderConfig {
    ProviderConfig {
        allowed_routes: vec![Route::Transcription, Route::Live],
        allowed_live_modes: vec![LiveMode::Interpretation],
        allow_tts: false,
        total_duration_seconds: 900,
        route_duration_seconds: 900,
        max_sessions: 2,
        max_tts_requests: 0,
        max_input_audio_seconds: 3600,
        max_output_audio_seconds: 1800,
        max_text_characters: text_limit,
        approved_budget_microusd: 1_500_000,
        ..config()
    }
}

fn protocols() -> (protocol::Protocol, protocol::Protocol) {
    let mut source = protocol::Protocol::new(Route::Transcription, Language::Ja);
    source
        .consume(json!({"type":"session.created","session":resolved(Route::Transcription)}))
        .unwrap();
    source
        .consume(json!({"type":"session.updated","session":resolved(Route::Transcription)}))
        .unwrap();
    assert!(source.asr.is_local());
    let mut live =
        protocol::Protocol::with_mode(Route::Live, Language::En, Some(LiveMode::Interpretation))
            .unwrap();
    live.consume(json!({"type":"session.started","session":resolved(Route::Live)}))
        .unwrap();
    (source, live)
}

// Exercise the actual local ASR commit ledger and both actual transcript
// parsers, then their production ingress accounting boundaries. Synthetic
// ordinals/receipts do not assert that any waveform was segmented correctly.
fn utterance(
    backend: &OpenAiBackend,
    source: &mut protocol::Protocol,
    live: &mut protocol::Protocol,
    ordinal: u64,
    seconds: u64,
    scalars: usize,
) -> Result<(), ProviderError> {
    let id = format!("synthetic_{ordinal}");
    let previous = (ordinal > 0).then(|| format!("synthetic_{}", ordinal - 1));
    let start = ordinal * seconds * 50;
    let end = start + seconds * 50;
    let mut at = start;
    while at < end {
        let next = (at + 15).min(end);
        source.asr.append_local(at, next)?;
        at = next;
    }
    source.frames = end;
    source.asr.commit_local(local_vad::CommitBoundary {
        start_frame: start,
        end_frame: end,
        reason: local_vad::CommitReason::Silence,
    })?;
    source.consume(
        json!({"type":"input_audio_buffer.committed","item_id":id,"previous_item_id":previous}),
    )?;
    let text: String = "日本語 हिन्दी English "
        .chars()
        .cycle()
        .take(scalars)
        .collect();
    assert_eq!(text.chars().count(), scalars);
    assert!(text.len() > scalars, "Unicode scalars are not UTF-8 bytes");
    for event in [
        json!({"type":"conversation.item.input_audio_transcription.delta","item_id":id,"content_index":0,"delta":text}),
        json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":text,"usage":{"type":"duration","seconds":seconds}}),
    ] {
        let output = source.consume(event)?;
        assert!(matches!(
            &output[..],
            [StreamEvent::Transcript {
                provenance: Provenance::SourceAsr,
                ..
            }]
        ));
        backend
            .shared
            .text_count(std::mem::take(&mut source.asr.text_charge))?;
    }
    assert!(source.asr.complete());
    for (kind, expected) in [
        ("session.input_transcript.delta", Provenance::LiveInput),
        ("session.output_transcript.delta", Provenance::LiveOutput),
    ] {
        let output =
            live.consume(json!({"type":kind,"delta":text,"start_ms":start*20,"end_ms":end*20}))?;
        let [
            StreamEvent::LiveTranscript {
                text, provenance, ..
            },
        ] = &output[..]
        else {
            panic!("Expected a transcript delta");
        };
        assert_eq!(*provenance, expected);
        backend.shared.text(text)?;
    }
    Ok(())
}

#[test]
fn old_3600_limit_reproduces_early_failure_without_exhausting_money_or_time() {
    let backend = OpenAiBackend::new(pair_config(3600)).unwrap();
    let mut source_reservation = backend
        .shared
        .reserve(Some(Route::Transcription), 0)
        .unwrap();
    let mut live_reservation = backend.shared.reserve(Some(Route::Live), 0).unwrap();
    let reserved = backend.budget_snapshot().amounts.unwrap().reserved_microusd;
    assert!(reserved <= 1_005_000);
    let source_stop = backend.shared.stop.subscribe();
    let live_stop = backend.shared.stop.subscribe();
    let (mut source, mut live) = protocols();
    for ordinal in 0..18 {
        utterance(&backend, &mut source, &mut live, ordinal, 1, 50).unwrap();
    }
    assert_eq!(backend.shared.state.lock().unwrap().text, 3600);
    assert!(backend.enabled());
    assert_eq!(
        utterance(&backend, &mut source, &mut live, 18, 1, 50),
        Err(ProviderError::TextLimit)
    );
    assert_eq!(ProviderError::TextLimit.code(), "text_limit");
    assert!(*source_stop.borrow() && *live_stop.borrow());
    let snapshot = backend.budget_snapshot();
    assert_eq!(snapshot.amounts.unwrap().reserved_microusd, reserved);
    assert_eq!(snapshot.stream_reservations.unwrap().used, 2);
    assert!(snapshot.remaining_seconds.unwrap() > 800);
    assert!(!backend.enabled());
    source_reservation.completed = true;
    live_reservation.completed = true;
}

#[test]
fn v3_handles_900_second_equivalent_multilingual_ingress_then_stops_at_exact_ceiling() {
    let backend = OpenAiBackend::new(pair_config(230400)).unwrap();
    let mut first = backend
        .shared
        .reserve(Some(Route::Transcription), 0)
        .unwrap();
    let mut second = backend.shared.reserve(Some(Route::Live), 0).unwrap();
    assert!(matches!(
        backend.shared.reserve(Some(Route::Live), 0),
        Err(ProviderError::StreamLimit)
    ));
    let reserved = backend.budget_snapshot().amounts.unwrap().reserved_microusd;
    // 75 twelve-second utterances, four text representations at48 scalars/s.
    // Source fan-out and generated audio counters are independent and bounded.
    backend.shared.input(900 * 2 * 16000).unwrap();
    backend.shared.output(900 * 32000, 16000).unwrap();
    let (mut source, mut live) = protocols();
    for ordinal in 0..75 {
        utterance(&backend, &mut source, &mut live, ordinal, 12, 576).unwrap();
    }
    assert_eq!(source.frames, 900 * 50);
    assert_eq!(backend.shared.state.lock().unwrap().text, 172800);
    assert!(backend.enabled());
    backend.shared.text_count(230400 - 172800).unwrap();
    assert!(backend.enabled(), "Exact ceiling is accepted");
    assert_eq!(backend.shared.text("語"), Err(ProviderError::TextLimit));
    assert!(!backend.enabled());
    assert_eq!(
        backend.budget_snapshot().amounts.unwrap().reserved_microusd,
        reserved
    );
    assert_eq!(
        backend.budget_snapshot().stream_reservations.unwrap().used,
        2
    );
    first.completed = true;
    second.completed = true;
}

#[test]
fn unicode_scalar_boundary_and_overflow_cannot_wrap_or_masquerade_as_spend() {
    let backend = OpenAiBackend::new(pair_config(5)).unwrap();
    let text = "日a\u{301}हि";
    assert_eq!(text.chars().count(), 5);
    assert!(text.len() > 5);
    backend.shared.text(text).unwrap();
    assert!(backend.enabled());
    assert_eq!(backend.shared.text("😀"), Err(ProviderError::TextLimit));
    assert_eq!(backend.shared.state.lock().unwrap().text, 6);
    assert!(!backend.enabled());
    assert_eq!(
        backend.budget_snapshot().amounts.unwrap().reserved_microusd,
        0
    );
    let backend = OpenAiBackend::new(pair_config(1_000_000)).unwrap();
    backend.shared.state.lock().unwrap().text = u64::MAX;
    assert_eq!(backend.shared.text_count(1), Err(ProviderError::TextLimit));
    assert_eq!(backend.shared.state.lock().unwrap().text, u64::MAX);
    assert!(!backend.enabled());
    assert_eq!(
        diagnostics::error_category(ProviderError::TextLimit),
        DiagnosticCategory::TextLimit
    );
    assert_eq!(
        serde_json::to_value(DiagnosticCategory::TextLimit).unwrap(),
        "text_limit"
    );
}

#[tokio::test]
async fn actual_live_transport_stops_before_emitting_over_limit_text_and_revokes_peer() {
    let backend = OpenAiBackend::new(pair_config(3600)).unwrap();
    backend.shared.text_count(3599).unwrap();
    let peer_stop = backend.shared.stop.subscribe();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (_input, input) = tokio::sync::mpsc::channel::<StreamInput>(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut protocol = protocol::Protocol::with_mode(
            Route::Live,
            Language::En,
            Some(LiveMode::Interpretation),
        )
        .unwrap();
        let report = transport::drive(
            &shared,
            &mut client,
            &mut protocol,
            input,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        // Exercise the same synchronous publisher called by production run().
        // Network handshake, hangup and final closure are not exercised here.
        transport::publish_failure(&events, &report);
        report
    });
    let startup = server.next().await.unwrap().unwrap();
    assert!(startup.is_text());
    server
        .send(Message::Text(
            json!({"type":"session.started","session":resolved(Route::Live)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Ready { .. })
    ));
    server
        .send(Message::Text(
            json!({"type":"session.output_transcript.delta","delta":"日","start_ms":0,"end_ms":20})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await, Some(StreamEvent::LiveTranscript { text, provenance: Provenance::LiveOutput, .. }) if text == "日")
    );
    server
        .send(Message::Text(
            json!({"type":"session.input_transcript.delta","delta":"語","start_ms":20,"end_ms":40})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let result = tokio::time::timeout(Duration::from_secs(3), task)
        .await
        .unwrap()
        .unwrap();
    assert_eq!(result, Err((false, ProviderError::TextLimit)));
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Error(ProviderError::TextLimit))
    ));
    assert!(
        output.recv().await.is_none(),
        "The overflowing fragment must not be published"
    );
    assert!(
        *peer_stop.borrow(),
        "Shared authority revokes the other stream"
    );
    assert!(!backend.enabled());
}
