//! Actual in-memory WebSocket transport with the pinned native detector.
//! All PCM is synthetic fixture data. No socket bind, key, provider or playback.
use super::*;
use crate::provider::local_vad::BoundaryError;
use std::sync::Mutex;
use tokio::sync::mpsc;

#[derive(Default)]
struct Wire {
    appends: usize,
    commits: usize,
    selected_frames: u64,
    startups: usize,
}

struct Finished {
    result: Result<(), (bool, ProviderError)>,
    captured: u64,
    appended: u64,
    charged_samples: u64,
    complete: bool,
}

struct Harness {
    input: mpsc::Sender<StreamInput>,
    output: mpsc::Receiver<StreamEvent>,
    stop: watch::Sender<bool>,
    wire: Arc<Mutex<Wire>>,
    release_finals: watch::Sender<bool>,
    drive: tokio::task::JoinHandle<Finished>,
    peer: tokio::task::JoinHandle<()>,
    _backend: OpenAiBackend,
}

impl Harness {
    async fn start(prestopped: bool) -> Self {
        Self::start_with_finals(prestopped, true).await
    }

    async fn start_with_finals(prestopped: bool, finals_available: bool) -> Self {
        let backend = OpenAiBackend::new(config()).unwrap();
        let shared = backend.shared.clone();
        let (mut client, mut server) = pair(65536).await;
        let (input, input_rx) = mpsc::channel(8);
        let (events, mut output) = mpsc::channel(32);
        let (stop, mut stopped) = watch::channel(prestopped);
        let mut global = shared.stop.subscribe();
        let wire = Arc::new(Mutex::new(Wire::default()));
        let seen = wire.clone();
        let (release_finals, mut finals) = watch::channel(finals_available);
        let peer = tokio::spawn(async move {
            let session = resolved(Route::Transcription);
            if !prestopped {
                server
                    .send(Message::Text(
                        json!({"type":"session.created","session":session})
                            .to_string()
                            .into(),
                    ))
                    .await
                    .unwrap();
            }
            while let Some(Ok(Message::Text(text))) = server.next().await {
                let event: Value = serde_json::from_str(&text).unwrap();
                match event["type"].as_str().unwrap() {
                    "session.update" => {
                        assert_eq!(event, protocol::start(Route::Transcription, Language::Ja));
                        {
                            let mut seen = seen.lock().unwrap();
                            seen.startups += 1;
                            assert_eq!(seen.startups, 1);
                        }
                        server
                            .send(Message::Text(
                                json!({"type":"session.updated","session":session})
                                    .to_string()
                                    .into(),
                            ))
                            .await
                            .unwrap();
                    }
                    "input_audio_buffer.append" => {
                        let bytes = STANDARD.decode(event["audio"].as_str().unwrap()).unwrap();
                        let mut seen = seen.lock().unwrap();
                        seen.appends += 1;
                        seen.selected_frames += bytes.len() as u64 / 960;
                        assert!(seen.appends <= 1000);
                    }
                    "input_audio_buffer.commit" => {
                        let number = {
                            let mut seen = seen.lock().unwrap();
                            seen.commits += 1;
                            seen.commits
                        };
                        let id = format!("source_{number}");
                        let previous = (number > 1).then(|| format!("source_{}", number - 1));
                        server
                            .send(Message::Text(
                                receipt(&id, previous.as_deref()).to_string().into(),
                            ))
                            .await
                            .unwrap();
                        // Hold a real pending final for input-queue tests. An
                        // already-final ledger no longer has a synthetic grace
                        // period in which to enqueue post-seal PCM.
                        while !*finals.borrow() {
                            tokio::select! {
                                result = finals.changed() => { if result.is_err() { return; } },
                                message = server.next() => {
                                    assert!(matches!(message, None | Some(Err(_)) | Some(Ok(Message::Close(_)))));
                                    return;
                                }
                            }
                        }
                        server
                            .send(Message::Text(
                                completed(&id, "fictional source").to_string().into(),
                            ))
                            .await
                            .unwrap();
                    }
                    other => panic!("unexpected ASR write: {other}"),
                }
            }
        });
        let drive = tokio::spawn(async move {
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
            let charged_samples = shared.state.lock().unwrap().input_samples;
            Finished {
                result,
                captured: p.captured_frames,
                appended: p.frames,
                charged_samples,
                complete: p.complete_input(),
            }
        });
        if !prestopped {
            assert!(matches!(
                tokio::time::timeout(Duration::from_secs(1), output.recv())
                    .await
                    .unwrap(),
                Some(StreamEvent::Ready { .. })
            ));
        }
        Self {
            input,
            output,
            stop,
            wire,
            release_finals,
            drive,
            peer,
            _backend: backend,
        }
    }

    async fn feed(&self, pcm: &[u8]) {
        self.input
            .send(StreamInput::Pcm16(pcm.to_vec()))
            .await
            .unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }

    async fn finish(self) -> Finished {
        let finished = tokio::time::timeout(Duration::from_secs(3), self.drive)
            .await
            .unwrap()
            .unwrap();
        tokio::time::timeout(Duration::from_secs(1), self.peer)
            .await
            .unwrap()
            .unwrap();
        finished
    }
}

fn signal_frames() -> Vec<Vec<u8>> {
    crate::test_audio::signal(80, 0)
        .chunks_exact(640)
        .map(<[u8]>::to_vec)
        .collect()
}

#[tokio::test]
async fn boundary_finish_waits_for_actual_detector_silence_then_drains_exactly_once() {
    let speech = signal_frames();
    let mut witness = LocalVad::new().unwrap();
    let mut expected_selected = 0;
    for frame in &speech {
        let segment = witness.push(frame).unwrap();
        expected_selected += segment.append.len() as u64 / 640;
        assert!(segment.commit.is_none());
    }
    assert!(witness.active());
    let mut silence_count = 0;
    loop {
        let segment = witness.push(&[0; 640]).unwrap();
        expected_selected += segment.append.len() as u64 / 640;
        silence_count += 1;
        assert!(silence_count <= 100);
        if segment.commit.is_some() {
            break;
        }
    }
    let mut harness = Harness::start_with_finals(false, false).await;
    for frame in &speech[..50] {
        harness.feed(frame).await;
    }
    harness
        .input
        .send(StreamInput::FinishAtBoundary)
        .await
        .unwrap();
    for frame in &speech[50..] {
        harness.feed(frame).await;
        assert!(
            harness.output.try_recv().is_err(),
            "a request is not a natural boundary"
        );
        assert_eq!(harness.wire.lock().unwrap().commits, 0);
    }
    for _ in 0..silence_count {
        harness.feed(&[0; 640]).await;
    }
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::InputSealed)
    ));
    // These are exactly the maximum in-flight entries in the child input queue.
    for _ in 0..8 {
        harness
            .input
            .send(StreamInput::Pcm16(vec![0; 640]))
            .await
            .unwrap();
    }
    harness.release_finals.send_replace(true);
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::Transcript {
            final_fragment: true,
            ..
        })
    ));
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    let wire = harness.wire.clone();
    let finished = harness.finish().await;
    assert_eq!(finished.result, Ok(()));
    assert!(finished.complete);
    assert_eq!(finished.captured, speech.len() as u64 + silence_count);
    assert_eq!(finished.appended, expected_selected);
    assert_eq!(finished.charged_samples, finished.captured * 320);
    assert_eq!(wire.lock().unwrap().selected_frames, expected_selected);
    assert_eq!(wire.lock().unwrap().commits, 1);
}

#[tokio::test]
async fn stop_cancels_a_pending_boundary_without_committing_active_speech() {
    let speech = signal_frames();
    let harness = Harness::start(false).await;
    for frame in &speech[..50] {
        harness.feed(frame).await;
    }
    harness
        .input
        .send(StreamInput::FinishAtBoundary)
        .await
        .unwrap();
    for frame in &speech[50..60] {
        harness.feed(frame).await;
    }
    harness.stop.send_replace(true);
    let wire = harness.wire.clone();
    let finished = harness.finish().await;
    assert_eq!(finished.result, Ok(()));
    assert!(!finished.complete);
    assert_eq!(finished.captured, 60);
    assert_eq!(wire.lock().unwrap().commits, 0);
    assert!(wire.lock().unwrap().appends > 0);
}

#[tokio::test]
async fn prelatched_stop_never_polls_a_queued_boundary_or_emits_an_empty_commit() {
    let harness = Harness::start(true).await;
    let _ = harness.input.try_send(StreamInput::FinishAtBoundary);
    let wire = harness.wire.clone();
    let finished = harness.finish().await;
    assert_eq!(finished.result, Ok(()));
    assert_eq!(finished.captured, 0);
    assert_eq!(wire.lock().unwrap().commits, 0);
    assert_eq!(wire.lock().unwrap().appends, 0);
}

#[tokio::test]
async fn inactive_speech_candidate_cannot_be_reported_as_complete_at_handover() {
    let speech = signal_frames();
    let candidate_end = (1..=speech.len())
        .find(|end| {
            let mut witness = LocalVad::new().unwrap();
            for frame in &speech[..*end] {
                witness.push(frame).unwrap();
            }
            !witness.active()
                && witness
                    .finish()
                    .is_err_and(|error| error == BoundaryError::IncompleteCandidate)
        })
        .expect("fixed speech fixture must contain an actual detector onset candidate");
    let harness = Harness::start(false).await;
    for frame in &speech[..candidate_end] {
        harness.feed(frame).await;
    }
    harness
        .input
        .send(StreamInput::FinishAtBoundary)
        .await
        .unwrap();
    let finished = harness.finish().await;
    assert_eq!(
        finished.result,
        Err((false, ProviderError::SourceIncomplete))
    );
    assert_eq!(finished.appended, 0);
}

#[tokio::test]
async fn post_seal_discard_is_limited_to_the_child_queue_capacity() {
    let mut harness = Harness::start_with_finals(false, false).await;
    let speech = signal_frames();
    let mut witness = LocalVad::new().unwrap();
    for frame in &speech {
        witness.push(frame).unwrap();
        harness.feed(frame).await;
    }
    assert!(witness.active());
    harness
        .input
        .send(StreamInput::FinishAtBoundary)
        .await
        .unwrap();
    let mut silence_count = 0;
    loop {
        silence_count += 1;
        assert!(silence_count <= 100);
        let segment = witness.push(&[0; 640]).unwrap();
        harness.feed(&[0; 640]).await;
        if segment.commit.is_some() {
            break;
        }
    }
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::InputSealed)
    ));
    for _ in 0..9 {
        let _ = harness.input.send(StreamInput::Pcm16(vec![0; 640])).await;
    }
    let finished = harness.finish().await;
    assert_eq!(
        finished.result,
        Err((false, ProviderError::ProtocolCapacityLimit))
    );
    assert_eq!(finished.captured, speech.len() as u64 + silence_count);
    assert_eq!(finished.charged_samples, finished.captured * 320);
    assert!(!finished.complete, "the held final must remain unresolved");
}

#[tokio::test]
async fn actual_silent_pcm_between_utterances_keeps_native_asr_transport_ready() {
    let mut harness = Harness::start(false).await;
    let speech = signal_frames();
    for frame in &speech {
        harness.feed(frame).await;
    }
    // Five seconds without speech is valid live audio, not a disconnected input.
    for _ in 0..250 {
        harness.feed(&[0; 640]).await;
    }
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::Transcript {
            final_fragment: true,
            ..
        })
    ));
    assert!(harness.output.try_recv().is_err());
    assert_eq!(harness.wire.lock().unwrap().commits, 1);
    for frame in &speech {
        harness.feed(frame).await;
    }
    for _ in 0..75 {
        harness.feed(&[0; 640]).await;
    }
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::Transcript {
            final_fragment: true,
            ..
        })
    ));
    assert_eq!(harness.wire.lock().unwrap().commits, 2);
    assert!(
        harness.output.try_recv().is_err(),
        "silence cannot produce closure or a synthetic commit"
    );
    harness.input.send(StreamInput::FinishInput).await.unwrap();
    assert!(matches!(
        harness.output.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    let finished = harness.finish().await;
    assert_eq!(finished.result, Ok(()));
    assert!(finished.complete);
    assert_eq!(finished.captured, 2 * speech.len() as u64 + 325);
}
