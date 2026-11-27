//! Receipt-driven source drain acceptance over actual in-memory WebSockets.
//! No network, provider, microphone, persistent session or generated playback.
use super::*;
use tokio::sync::mpsc;
use tokio_tungstenite::WebSocketStream;

type DriveResult = (Result<(), (bool, ProviderError)>, bool);

struct Harness {
    server: WebSocketStream<tokio::io::DuplexStream>,
    input: mpsc::Sender<StreamInput>,
    output: mpsc::Receiver<StreamEvent>,
    stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<DriveResult>,
    _backend: OpenAiBackend,
}

impl Harness {
    async fn start(mut protocol: protocol::Protocol, lifetime: Duration, prestopped: bool) -> Self {
        let backend = OpenAiBackend::new(config()).unwrap();
        let shared = backend.shared.clone();
        let (mut client, server) = pair(65536).await;
        let (input, input_rx) = mpsc::channel(8);
        let (events, output) = mpsc::channel(32);
        let (stop, mut stopped) = watch::channel(prestopped);
        let mut global = shared.stop.subscribe();
        let hard = Instant::now() + lifetime + Duration::from_secs(10);
        let task = tokio::spawn(async move {
            let result = transport::drive(
                &shared,
                &mut client,
                &mut protocol,
                input_rx,
                &events,
                &mut stopped,
                &mut global,
                hard,
            )
            .await;
            (result, protocol.complete_input())
        });
        Self {
            server,
            input,
            output,
            stop,
            task,
            _backend: backend,
        }
    }

    async fn send(&mut self, value: Value) {
        self.server
            .send(Message::Text(value.to_string().into()))
            .await
            .unwrap();
    }

    async fn next(&mut self) -> StreamEvent {
        tokio::time::timeout(Duration::from_secs(3), self.output.recv())
            .await
            .expect("bounded synthetic provider observation")
            .expect("expected event")
    }

    async fn finish(self, complete: bool) {
        let (result, observed) = tokio::time::timeout(Duration::from_secs(3), self.task)
            .await
            .unwrap()
            .unwrap();
        assert_eq!(result, Ok(()));
        assert_eq!(observed, complete);
    }
}

fn committed(count: u64) -> protocol::Protocol {
    let mut protocol = manual_ready();
    for index in 0..count {
        commit(&mut protocol.asr, index * 20, (index + 1) * 20);
    }
    protocol.frames = count * 20;
    protocol
}

fn is_final(event: &StreamEvent, sequence: u32) -> bool {
    matches!(event, StreamEvent::Transcript { final_fragment: true, item: Some(item), .. } if item.sequence == sequence)
}

fn is_complete(event: &StreamEvent) -> bool {
    matches!(
        event,
        StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        }
    )
}

#[test]
fn source_cutoff_dominates_a_parsed_final_batch_that_cannot_be_fully_published() {
    let mut protocol = committed(2);
    protocol.consume(receipt("source_a", None)).unwrap();
    protocol
        .consume(receipt("source_b", Some("source_a")))
        .unwrap();
    assert!(
        protocol
            .consume(completed("source_b", "later source"))
            .unwrap()
            .is_empty()
    );
    // Parsing the earlier final drains both ordered source items from the
    // protocol ledger before the transport can publish either event. Model
    // the incoming path observing its shorter drain cutoff before/between
    // those publications, without racing a wall-clock nanosecond in CI.
    let output = protocol
        .consume(completed("source_a", "earlier source"))
        .unwrap();
    assert_eq!(output.len(), 2);
    assert!(is_final(&output[0], 1));
    assert!(is_final(&output[1], 2));
    assert!(protocol.complete_input());
    let end = Instant::now();
    for published in [0, 1] {
        assert!(output[published..].iter().any(|event| is_final(event, 2)));
        for now in [end, end + Duration::from_nanos(1)] {
            assert_eq!(
                transport::finishing_completion(now, end, protocol.complete_input()),
                Some(DrainCompletion::ObservationWindowEnded),
                "a complete ledger is insufficient once final publication was cut off"
            );
        }
    }
    assert_eq!(
        transport::finishing_completion(end - Duration::from_nanos(1), end, true),
        Some(DrainCompletion::CommittedTranscriptsFinal)
    );
    assert_eq!(
        transport::finishing_completion(end - Duration::from_nanos(1), end, false),
        None
    );
    assert_eq!(
        transport::finishing_completion(end, end, false),
        Some(DrainCompletion::ObservationWindowEnded)
    );
}

#[tokio::test]
async fn source_drain_has_no_extra_settling_second_after_final_or_finish() {
    let mut after_final_ms = Vec::new();
    let mut after_finish_ms = Vec::new();
    for _ in 0..3 {
        let mut run = Harness::start(committed(1), Duration::from_secs(10), false).await;
        run.input.send(StreamInput::FinishInput).await.unwrap();
        run.send(receipt("source_a", None)).await;
        run.send(delta("source_a", "provisional")).await;
        assert!(matches!(
            run.next().await,
            StreamEvent::Transcript {
                final_fragment: false,
                ..
            }
        ));
        assert!(
            tokio::time::timeout(Duration::from_millis(30), run.output.recv())
                .await
                .is_err()
        );
        let before_final = Instant::now();
        run.send(completed("source_a", "final source")).await;
        assert!(
            is_final(&run.next().await, 1),
            "final text must precede Drained"
        );
        assert!(is_complete(&run.next().await));
        after_final_ms.push(before_final.elapsed().as_secs_f64() * 1000.0);
        run.finish(true).await;

        let mut run = Harness::start(committed(1), Duration::from_secs(10), false).await;
        run.send(receipt("source_a", None)).await;
        run.send(completed("source_a", "final source")).await;
        assert!(is_final(&run.next().await, 1));
        assert!(
            run.output.try_recv().is_err(),
            "final alone must not seal input"
        );
        let before_finish = Instant::now();
        run.input.send(StreamInput::FinishInput).await.unwrap();
        assert!(is_complete(&run.next().await));
        after_finish_ms.push(before_finish.elapsed().as_secs_f64() * 1000.0);
        run.finish(true).await;
    }
    println!(
        "source_drain_timings={}",
        json!({"after_final_ms":after_final_ms,"after_finish_ms":after_finish_ms,"transport":"in_memory_websocket","provider_calls":0})
    );
    assert!(
        after_final_ms
            .iter()
            .chain(&after_finish_ms)
            .all(|ms| *ms < 250.0),
        "an already final, sealed local ASR ledger must not wait an extra settling second"
    );
}

#[tokio::test]
async fn source_drain_waits_for_each_out_of_order_final_and_preserves_source_order() {
    let mut run = Harness::start(committed(2), Duration::from_secs(10), false).await;
    run.input.send(StreamInput::FinishInput).await.unwrap();
    run.send(receipt("source_a", None)).await;
    run.send(receipt("source_b", Some("source_a"))).await;
    run.send(completed("source_b", "later source")).await;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), run.output.recv())
            .await
            .is_err()
    );
    run.send(completed("source_a", "earlier source")).await;
    assert!(is_final(&run.next().await, 1));
    assert!(is_final(&run.next().await, 2));
    assert!(is_complete(&run.next().await));
    run.finish(true).await;
}

#[tokio::test]
async fn source_drain_never_promotes_a_missing_receipt_or_final() {
    for has_receipt in [false, true] {
        let mut run = Harness::start(committed(1), Duration::from_millis(350), false).await;
        run.input.send(StreamInput::FinishInput).await.unwrap();
        if has_receipt {
            run.send(receipt("source_a", None)).await;
        }
        assert!(
            tokio::time::timeout(Duration::from_millis(100), run.output.recv())
                .await
                .is_err()
        );
        assert!(matches!(
            run.next().await,
            StreamEvent::Drained {
                completion: DrainCompletion::ObservationWindowEnded
            }
        ));
        run.finish(false).await;
    }
}

#[tokio::test]
async fn source_drain_never_promotes_an_unbound_draft_after_other_items_finish() {
    let mut run = Harness::start(committed(1), Duration::from_millis(350), false).await;
    run.send(delta("unbound", "not an ordinal")).await;
    assert!(matches!(
        run.next().await,
        StreamEvent::SourceDraft { retired: false, .. }
    ));
    run.input.send(StreamInput::FinishInput).await.unwrap();
    run.send(receipt("source_a", None)).await;
    run.send(completed("source_a", "bound source")).await;
    assert!(is_final(&run.next().await, 1));
    assert!(
        tokio::time::timeout(Duration::from_millis(100), run.output.recv())
            .await
            .is_err()
    );
    assert!(matches!(
        run.next().await,
        StreamEvent::Drained {
            completion: DrainCompletion::ObservationWindowEnded
        }
    ));
    run.finish(false).await;
}

#[tokio::test]
async fn source_drain_receipt_retires_draft_then_publishes_canonical_final_before_complete() {
    let mut run = Harness::start(committed(1), Duration::from_secs(10), false).await;
    run.send(delta("source_a", "draft source")).await;
    assert!(matches!(
        run.next().await,
        StreamEvent::SourceDraft { retired: false, .. }
    ));
    run.input.send(StreamInput::FinishInput).await.unwrap();
    run.send(receipt("source_a", None)).await;
    assert!(matches!(
        run.next().await,
        StreamEvent::SourceDraft { retired: true, .. }
    ));
    assert!(matches!(
        run.next().await,
        StreamEvent::Transcript {
            final_fragment: false,
            ..
        }
    ));
    assert!(
        tokio::time::timeout(Duration::from_millis(30), run.output.recv())
            .await
            .is_err()
    );
    run.send(completed("source_a", "corrected source")).await;
    assert!(is_final(&run.next().await, 1));
    assert!(is_complete(&run.next().await));
    run.finish(true).await;
}

#[tokio::test]
async fn source_drain_stop_and_expiry_win_over_a_complete_queued_finish() {
    for (lifetime, prestopped) in [(Duration::from_secs(10), true), (Duration::ZERO, false)] {
        let mut run = Harness::start(manual_ready(), lifetime, prestopped).await;
        let _ = run.input.try_send(StreamInput::FinishInput);
        assert!(
            run.output.recv().await.is_none(),
            "negative authority must not publish Drained"
        );
        run.finish(true).await;
    }
}

#[tokio::test]
async fn source_drain_stop_after_seal_suppresses_late_final_and_completion() {
    let mut run = Harness::start(committed(1), Duration::from_secs(10), false).await;
    run.input.send(StreamInput::FinishInput).await.unwrap();
    run.send(receipt("source_a", None)).await;
    run.send(delta("source_a", "still pending")).await;
    assert!(matches!(
        run.next().await,
        StreamEvent::Transcript {
            final_fragment: false,
            ..
        }
    ));
    run.stop.send_replace(true);
    let _ = run
        .server
        .send(Message::Text(
            completed("source_a", "must stay hidden").to_string().into(),
        ))
        .await;
    assert!(run.output.recv().await.is_none());
    run.finish(false).await;
}
