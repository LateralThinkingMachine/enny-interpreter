//! Actual production VAD/resampler/ASR transport with synthetic usage receipts.
//! No network sockets, API credentials, provider service or audio playback.
use super::*;

fn frames() -> Vec<Vec<u8>> {
    let pcm = crate::test_audio::signal(80, 0);
    let speech: Vec<_> = pcm.chunks_exact(640).map(<[u8]>::to_vec).collect();
    assert_eq!(speech.len(), 80);
    let mut output = Vec::new();
    for _ in 0..3 {
        output.extend(speech.iter().cloned());
        output.extend((0..100).map(|_| vec![0; 640]));
    }
    output
}

#[tokio::test]
async fn actual_selected_pcm_and_per_item_usage_survive_repeated_natural_commits() {
    let frames = frames();
    let mut witness = LocalVad::new().unwrap();
    let mut selected = 0_u64;
    let mut commits = 0;
    for frame in &frames {
        let segment = witness.push(frame).unwrap();
        selected += segment.append.len() as u64 / 640;
        commits += usize::from(segment.commit.is_some());
    }
    assert_eq!(commits, 3);
    assert!(witness.finish().unwrap().commit.is_none());

    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let captured = frames.len() as u64;
    let drive = tokio::spawn(async move {
        let mut protocol = protocol::Protocol::new(Route::Transcription, Language::Ja);
        transport::drive(
            &shared,
            &mut client,
            &mut protocol,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await
        .unwrap();
        assert_eq!(protocol.captured_frames, captured);
        assert_eq!(protocol.frames, selected);
        assert_eq!(shared.state.lock().unwrap().input_samples, captured * 320);
        assert!(protocol.complete_input());
        assert!((protocol.usage - (selected as f64 / 50.0 + 2.7)).abs() < 0.000_001);
        transport::close(
            &mut client,
            &mut protocol,
            Instant::now() + Duration::from_secs(1),
        )
        .await
    });
    let peer = tokio::spawn(async move {
        let session = resolved(Route::Transcription);
        server
            .send(Message::Text(
                json!({"type":"session.created","session":session})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        let mut uncommitted_frames = 0_u64;
        let mut committed = 0;
        while let Some(Ok(message)) = server.next().await {
            match message {
                Message::Text(text) => {
                    let event: Value = serde_json::from_str(&text).unwrap();
                    match event["type"].as_str().unwrap() {
                        "session.update" => {
                            assert_eq!(event, protocol::start(Route::Transcription, Language::Ja));
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
                            assert!(bytes.len().is_multiple_of(960));
                            uncommitted_frames += bytes.len() as u64 / 960;
                        }
                        "input_audio_buffer.commit" => {
                            committed += 1;
                            assert!(committed <= 3 && uncommitted_frames > 0);
                            let id = format!("source_{committed}");
                            let previous =
                                (committed > 1).then(|| format!("source_{}", committed - 1));
                            server
                                .send(Message::Text(
                                    receipt(&id, previous.as_deref()).to_string().into(),
                                ))
                                .await
                                .unwrap();
                            // Synthetic per-item discrepancy within existing
                            // local tolerance, not claimed provider rounding.
                            let reported = uncommitted_frames as f64 / 50.0 + 0.9;
                            server.send(Message::Text(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":"Fictional source phrase.","usage":{"type":"duration","seconds":reported}}).to_string().into())).await.unwrap();
                            uncommitted_frames = 0;
                        }
                        other => panic!("unexpected synthetic client event: {other}"),
                    }
                }
                Message::Close(_) => {
                    server.flush().await.unwrap();
                    break;
                }
                _ => panic!("unexpected synthetic client frame"),
            }
        }
        assert_eq!(committed, 3);
        assert_eq!(uncommitted_frames, 0);
    });
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
    for sequence in 1..=3 {
        assert!(matches!(output.recv().await, Some(StreamEvent::Transcript {
            final_fragment:true, item:Some(item), ..
        }) if item.sequence == sequence));
    }
    let _input = feeder.await.unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    assert!(drive.await.unwrap());
    peer.await.unwrap();
}
