//! Independent in-memory generated-output drain tests. No provider or devices.
use super::*;
use crate::engine::{DrainCompletion, StreamInput};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

type Peer = WebSocketStream<tokio::io::DuplexStream>;
type RunOutcome = (Result<(), (bool, ProviderError)>, bool);

struct TranslationRun {
    _backend: OpenAiBackend,
    server: Peer,
    input: tokio::sync::mpsc::Sender<StreamInput>,
    output: tokio::sync::mpsc::Receiver<StreamEvent>,
    stop: watch::Sender<bool>,
    shared: Arc<Shared>,
    task: tokio::task::JoinHandle<RunOutcome>,
}

async fn next_json(server: &mut Peer) -> Value {
    let incoming = tokio::time::timeout(Duration::from_secs(2), server.next())
        .await
        .expect("bounded upstream command")
        .expect("open peer")
        .expect("valid frame");
    serde_json::from_str(incoming.to_text().expect("JSON command")).unwrap()
}

async fn send(server: &mut Peer, value: Value) {
    server
        .send(Message::Text(value.to_string().into()))
        .await
        .unwrap();
}

async fn translation(active: Duration) -> TranslationRun {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let owner = shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(8);
    let (stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let hard = Instant::now() + active + Duration::from_secs(10);
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Translation, Language::Ja);
        let result = transport::drive(
            &owner,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            hard,
        )
        .await;
        let confirmed =
            transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1)).await;
        (result, confirmed)
    });
    send(
        &mut server,
        json!({"type":"session.created","session":resolved(Route::Translation)}),
    )
    .await;
    let configure = next_json(&mut server).await;
    assert_eq!(configure["type"], "session.update");
    send(
        &mut server,
        json!({"type":"session.updated","session":resolved(Route::Translation)}),
    )
    .await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Ready { .. })
    ));
    TranslationRun {
        _backend: backend,
        server,
        input,
        output,
        stop,
        shared,
        task,
    }
}

fn terminal() -> Value {
    // The translation API's closed event has no session/model fields. It is
    // scoped to the already validated dedicated stream, not a reused Live DTO.
    json!({"type":"session.closed","event_id":"synthetic_closed"})
}

fn tail_audio() -> Value {
    json!({"type":"session.output_audio.delta","delta":STANDARD.encode([1u8; 960]),"sample_rate":24000})
}

fn tail_text() -> Value {
    json!({"type":"session.output_transcript.delta","delta":"retained final translation tail"})
}

#[tokio::test]
async fn translation_flush_command_precedes_retained_audio_text_and_confirmed_drain() {
    let mut run = translation(Duration::from_secs(5)).await;
    run.input.send(StreamInput::FinishInput).await.unwrap();
    assert_eq!(
        next_json(&mut run.server).await,
        json!({"type":"session.close"})
    );
    assert!(
        run.output.try_recv().is_err(),
        "sending a close is not a finalization receipt"
    );
    // The fake provider emits nothing until the actual documented close command.
    send(&mut run.server, tail_audio()).await;
    send(&mut run.server, tail_text()).await;
    let audio = tokio::time::timeout(Duration::from_secs(1), run.output.recv())
        .await
        .unwrap();
    assert!(
        matches!(audio, Some(StreamEvent::Audio { pcm16, sample_rate:24000 }) if pcm16 == vec![1;960])
    );
    let text = tokio::time::timeout(Duration::from_secs(1), run.output.recv())
        .await
        .unwrap();
    assert!(
        matches!(text, Some(StreamEvent::Transcript { text, provenance:Provenance::Translation, final_fragment:false, item:None }) if text == "retained final translation tail")
    );
    assert!(
        run.output.try_recv().is_err(),
        "retained tail is still not a closed receipt"
    );
    send(&mut run.server, terminal()).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), run.output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Drained {
            completion: DrainCompletion::ProviderSessionClosed
        })
    ));
    let (result, confirmed) = run.task.await.unwrap();
    assert!(result.is_ok() && confirmed);
    {
        let ledger = run.shared.state.lock().unwrap();
        assert_eq!(
            ledger.input_samples, 0,
            "translation flush adds no fabricated input clock"
        );
        assert_eq!(ledger.output_samples_24k, 480);
        assert_eq!(ledger.text, "retained final translation tail".len() as u64);
    }
    assert!(
        !matches!(run.server.next().await, Some(Ok(Message::Text(_)))),
        "cleanup repeated a finalized close command"
    );
}

#[tokio::test]
async fn local_stop_during_translation_flush_hides_tail_and_suppresses_drained() {
    let mut run = translation(Duration::from_secs(5)).await;
    run.input.send(StreamInput::FinishInput).await.unwrap();
    assert_eq!(
        next_json(&mut run.server).await,
        json!({"type":"session.close"})
    );
    run.stop.send_replace(true);
    send(&mut run.server, tail_audio()).await;
    send(&mut run.server, tail_text()).await;
    send(&mut run.server, terminal()).await;
    let (result, confirmed) = run.task.await.unwrap();
    assert!(
        result.is_ok() && confirmed,
        "resource retirement is separate from local output acceptance"
    );
    assert!(
        run.output.recv().await.is_none(),
        "Stop must suppress both content and positive Drained events"
    );
    assert!(
        !matches!(run.server.next().await, Some(Ok(Message::Text(_)))),
        "Stop cleanup repeated session.close"
    );
}

#[tokio::test]
async fn expired_translation_drain_does_not_retry_close_or_release_late_content() {
    let mut run = translation(Duration::from_millis(1500)).await;
    run.input.send(StreamInput::FinishInput).await.unwrap();
    assert_eq!(
        next_json(&mut run.server).await,
        json!({"type":"session.close"})
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), run.output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Drained {
            completion: DrainCompletion::ObservationWindowEnded
        })
    ));
    // Cleanup waits for the first close request's receipt; no second command.
    assert!(
        tokio::time::timeout(Duration::from_millis(50), run.server.next())
            .await
            .is_err()
    );
    send(&mut run.server, tail_audio()).await;
    send(&mut run.server, tail_text()).await;
    send(&mut run.server, terminal()).await;
    let (result, confirmed) = run.task.await.unwrap();
    assert!(result.is_ok() && confirmed);
    assert!(
        run.output.recv().await.is_none(),
        "expired positive authority must not be resurrected by late close output"
    );
    assert!(!matches!(
        run.server.next().await,
        Some(Ok(Message::Text(_)))
    ));
}

#[tokio::test]
async fn unrequested_translation_close_never_claims_successful_finish() {
    let mut run = translation(Duration::from_secs(5)).await;
    send(&mut run.server, terminal()).await;
    let (result, confirmed) = run.task.await.unwrap();
    assert!(result.is_ok() && confirmed);
    assert!(
        run.output.recv().await.is_none(),
        "spontaneous closure did not acknowledge a FinishInput operation"
    );
    assert!(!matches!(
        run.server.next().await,
        Some(Ok(Message::Text(_)))
    ));
}
