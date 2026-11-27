//! Actual in-memory WebSocket scheduling; no provider, key lookup or devices.
use super::*;
use crate::engine::StreamInput;
use futures_util::{SinkExt, StreamExt};
use std::{
    io,
    pin::Pin,
    sync::atomic::{AtomicBool, Ordering},
};
use tokio::io::{AsyncRead, AsyncWrite, DuplexStream, ReadBuf};
use tokio::sync::mpsc;
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, protocol::Role},
};

// Observe the already admitted output count at the precise input transport
// write, rather than timing when the synthetic server task happens to run.
struct ObservedStream {
    io: DuplexStream,
    shared: Arc<Shared>,
    observe: Arc<AtomicBool>,
    writes: Arc<Mutex<Vec<u64>>>,
}
impl AsyncRead for ObservedStream {
    fn poll_read(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_read(cx, buf)
    }
}
impl AsyncWrite for ObservedStream {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        let result = Pin::new(&mut self.io).poll_write(cx, bytes);
        if matches!(result, Poll::Ready(Ok(n)) if n>0) && self.observe.load(Ordering::SeqCst) {
            self.writes
                .lock()
                .unwrap()
                .push(self.shared.state.lock().unwrap().output_samples_24k / 480);
        }
        result
    }
    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_flush(cx)
    }
    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        Pin::new(&mut self.io).poll_shutdown(cx)
    }
}

#[tokio::test(flavor = "current_thread")]
async fn live_input_advances_during_an_already_ready_output_burst() {
    exercise_burst(None, false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn versioned_interpreter_sends_reviewed_policy_and_keeps_input_during_output() {
    exercise_burst(None, true).await;
}

#[tokio::test(flavor = "current_thread")]
async fn live_duplex_media_never_precedes_stop_global_revocation_or_expiry() {
    for fence in ["local", "global", "external", "expiry"] {
        exercise_burst(Some(fence), false).await;
    }
}

async fn exercise_burst(fence: Option<&str>, versioned: bool) {
    let policy: crate::engine::Interpretation = serde_json::from_value(json!({
        "schema_version":"interpretation.v1", "mode":"two_way", "first":"ja", "second":"en"
    }))
    .unwrap();
    let (external_stop, external_receiver) = watch::channel(false);
    let backend =
        OpenAiBackend::new_with_controls(config(), vec![external_receiver], None).unwrap();
    let shared = backend.shared.clone();
    let observe = Arc::new(AtomicBool::new(false));
    let writes = Arc::new(Mutex::new(Vec::new()));
    let (client_io, server_io) = tokio::io::duplex(65536);
    let mut client = WebSocketStream::from_raw_socket(
        ObservedStream {
            io: client_io,
            shared: shared.clone(),
            observe: observe.clone(),
            writes: writes.clone(),
        },
        Role::Client,
        None,
    )
    .await;
    let mut server = WebSocketStream::from_raw_socket(server_io, Role::Server, None).await;
    let (input, input_rx) = mpsc::channel(8);
    let (events, mut output) = mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task_shared = shared.clone();
    let hard = Instant::now() + Duration::from_secs(if fence == Some("expiry") { 11 } else { 30 });
    let task = tokio::spawn(async move {
        let mut protocol = if versioned {
            protocol::Protocol::with_mode(
                Route::Live,
                Language::Ja,
                Some(crate::engine::LiveMode::Interpretation),
            )
            .unwrap()
        } else {
            protocol::Protocol::new(Route::Live, Language::Ja)
        };
        if versioned {
            protocol.configure_interpretation(policy).unwrap();
        }
        let result = transport::drive(
            &task_shared,
            &mut client,
            &mut protocol,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            hard,
        )
        .await;
        (result, protocol.captured_frames)
    });
    let startup = server.next().await.unwrap().unwrap().into_text().unwrap();
    let startup: Value = serde_json::from_str(&startup).unwrap();
    assert_eq!(startup["type"], "session.start");
    if versioned {
        assert_eq!(startup["session"]["instructions"], policy.instructions());
        assert!(
            startup["session"]["instructions"]
                .as_str()
                .unwrap()
                .contains(crate::interpretation::INTERPRETER_CONVERSATION_POLICY_V3)
        );
        assert_eq!(startup["session"]["model"], "gpt-live-1");
        assert_eq!(startup["session"]["delegation"], json!({"type":"client"}));
        assert_eq!(startup["session"]["store"], false);
    } else {
        assert!(
            !startup["session"]["instructions"]
                .as_str()
                .unwrap()
                .contains(crate::interpretation::INTERPRETER_CONVERSATION_POLICY_V3),
            "the localized legacy prompt remains unchanged"
        );
    }
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
    observe.store(true, Ordering::SeqCst);

    // Each send fits in the owned duplex buffer, so no other task runs before
    // both lanes are ready. This is a network burst, not a toy select loop.
    for _ in 0..16 {
        server
            .send(Message::Text(
                json!({"type":"session.output_audio.delta","delta":STANDARD.encode([7;640])})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
    }
    for frame in 0..8_u8 {
        input
            .try_send(StreamInput::Pcm16(vec![frame; 640]))
            .unwrap();
    }
    if let Some(fence) = fence {
        // The single-thread fixture has not yielded since queuing both lanes.
        // Each authority boundary must win before either media lane is read.
        match fence {
            "local" => {
                stop.send_replace(true);
            }
            "global" => {
                shared.stop.send_replace(true);
            }
            "external" => {
                external_stop.send_replace(true);
            }
            "expiry" => std::thread::sleep(
                (hard - Duration::from_secs(10)).saturating_duration_since(Instant::now())
                    + Duration::from_millis(2),
            ),
            _ => unreachable!(),
        }
        let (result, captured) = task.await.unwrap();
        assert!(result.is_ok(), "{fence}");
        assert_eq!(captured, 0, "{fence}");
        assert!(
            writes.lock().unwrap().is_empty(),
            "{fence} precedes input transport writes"
        );
        assert_eq!(
            shared.state.lock().unwrap().output_samples_24k,
            0,
            "{fence} precedes output publication"
        );
        return;
    }
    for frame in 0..8_u8 {
        let text = tokio::time::timeout(Duration::from_secs(2), server.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap()
            .into_text()
            .unwrap();
        let value: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(value["type"], "session.input_audio.append");
        assert_eq!(
            STANDARD.decode(value["audio"].as_str().unwrap()).unwrap(),
            vec![frame; 640]
        );
    }
    stop.send_replace(true);
    let (result, captured) = task.await.unwrap();
    assert!(result.is_ok());
    assert_eq!(captured, 8);
    let writes = writes.lock().unwrap().clone();
    assert_eq!(
        writes.len(),
        8,
        "each bounded PCM append fits one transport write"
    );
    for (index, before) in writes.iter().enumerate() {
        assert!(
            *before >= index as u64,
            "model output was starved before input frame {index}: only{before} outputs"
        );
        assert!(
            *before <= index as u64 + 1,
            "input frame {index} waited behind {before} model frames"
        );
    }
}
