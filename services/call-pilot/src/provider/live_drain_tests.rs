//! Offline clock-dependent Live transport acceptance. No provider or devices.
use super::*;
use crate::engine::{DrainCompletion, StreamInput};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

async fn read_json<S: tokio::io::AsyncRead + tokio::io::AsyncWrite + Unpin>(
    socket: &mut tokio_tungstenite::WebSocketStream<S>,
) -> Value {
    serde_json::from_str(
        &tokio::time::timeout(Duration::from_secs(3), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap(),
    )
    .unwrap()
}

#[tokio::test]
async fn live_finish_clock_drives_withheld_tail_and_keeps_source_accounting_separate() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let task_shared = shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let hard = Instant::now() + Duration::from_secs(12);
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        transport::drive(
            &task_shared,
            &mut client,
            &mut p,
            rx,
            &events,
            &mut stopped,
            &mut global,
            hard,
        )
        .await
        .unwrap();
        assert_eq!(
            p.captured_frames, 1,
            "local clock is never recorded as captured source"
        );
        let appended = p.frames;
        assert!(transport::close(&mut client, &mut p, hard).await);
        appended
    });
    assert_eq!(read_json(&mut server).await["type"], "session.start");
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
    input.send(StreamInput::Pcm16(vec![1; 640])).await.unwrap();
    let source = read_json(&mut server).await;
    assert_eq!(
        STANDARD.decode(source["audio"].as_str().unwrap()).unwrap(),
        vec![1; 640]
    );
    input.send(StreamInput::FinishInput).await.unwrap();
    let mut clock_frames = 0;
    let clock_start = Instant::now();
    loop {
        let packet = read_json(&mut server).await;
        if packet["type"] == "session.close" {
            break;
        }
        assert_eq!(packet["type"], "session.input_audio.append");
        assert_eq!(
            STANDARD.decode(packet["audio"].as_str().unwrap()).unwrap(),
            vec![0; 640]
        );
        clock_frames += 1;
        assert!(clock_frames <= clock_start.elapsed().as_millis() as u64 / 20 + 1);
        // This synthetic server cannot advance its generated tail on a wall
        // clock alone. Only the third post-EOF model-clock frame unlocks it.
        if clock_frames == 3 {
            server.send(Message::Text(json!({"type":"session.output_transcript.delta","delta":"tail after input EOF","start_ms":0,"end_ms":60}).to_string().into())).await.unwrap();
            server
                .send(Message::Text(
                    json!({"type":"session.output_audio.delta","delta":STANDARD.encode([2;640])})
                        .to_string()
                        .into(),
                ))
                .await
                .unwrap();
            assert!(
                matches!(output.recv().await,Some(StreamEvent::LiveTranscript{text,..}) if text=="tail after input EOF")
            );
            assert!(
                matches!(output.recv().await,Some(StreamEvent::Audio{pcm16,..}) if pcm16==vec![2;640])
            );
        }
    }
    assert!((3..=100).contains(&clock_frames));
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::ObservationWindowEnded
        })
    ));
    server.send(Message::Text(json!({"type":"session.closed","session":{"id":"sess_synthetic","model":protocol::LIVE},"reason":"close_requested","usage":{"seconds":2}}).to_string().into())).await.unwrap();
    assert_eq!(task.await.unwrap(), clock_frames + 1);
    assert_eq!(
        shared.state.lock().unwrap().input_samples,
        (clock_frames + 1) * 320
    );
}

#[tokio::test(flavor = "current_thread")]
async fn live_clock_stop_after_stalled_timer_never_polls_another_positive_write() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let task_shared = shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        let result = transport::drive(
            &task_shared,
            &mut client,
            &mut p,
            rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert!(result.is_ok());
        assert_eq!(p.captured_frames, 0);
        p.frames
    });
    assert_eq!(read_json(&mut server).await["type"], "session.start");
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
    input.send(StreamInput::FinishInput).await.unwrap();
    assert_eq!(
        read_json(&mut server).await["type"],
        "session.input_audio.append"
    );
    // Block this runtime's timer driver across five would-be clock ticks, then
    // latch Stop before it can resume. No timer catch-up or transport repoll.
    std::thread::sleep(Duration::from_millis(110));
    stop.send_replace(true);
    assert_eq!(task.await.unwrap(), 1);
    assert_eq!(shared.state.lock().unwrap().input_samples, 320);
    assert!(
        output.recv().await.is_none(),
        "Stop produces no Drained or late content"
    );
}

#[tokio::test]
async fn generated_live_clock_exhausts_input_budget_before_any_unreserved_append() {
    let mut cfg = config();
    cfg.max_input_audio_seconds = 1;
    let backend = OpenAiBackend::new(cfg).unwrap();
    let shared = backend.shared.clone();
    let task_shared = shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        let result = transport::drive(
            &task_shared,
            &mut client,
            &mut p,
            rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert_eq!(result, Err((false, ProviderError::InputAudioLimit)));
        assert_eq!(p.captured_frames, 0);
        assert_eq!(p.frames, 50);
    });
    assert_eq!(read_json(&mut server).await["type"], "session.start");
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
    input.send(StreamInput::FinishInput).await.unwrap();
    for _ in 0..50 {
        assert_eq!(
            read_json(&mut server).await["type"],
            "session.input_audio.append"
        );
    }
    task.await.unwrap();
    assert!(shared.state.lock().unwrap().uncertain);
    assert!(!shared.available());
    assert!(output.recv().await.is_none());
}
