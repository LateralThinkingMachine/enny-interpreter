//! Independent manual-boundary acceptance: synthetic state and in-memory
//! WebSockets only. No credentials, sockets, provider calls or device capture.
use super::super::{
    asr::Asr,
    local_vad::{CommitBoundary, CommitReason, LocalVad, Segmenter},
};
use super::*;
use crate::engine::{DrainCompletion, StreamInput};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

#[path = "boundary_handover_tests.rs"]
mod boundary_handover_tests;

#[path = "asr_usage_transport_tests.rs"]
mod asr_usage_transport_tests;

#[path = "receipt_drain_tests.rs"]
mod receipt_drain_tests;

fn manual_ready() -> protocol::Protocol {
    let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
    let mut session = resolved(Route::Transcription);
    session["audio"]["input"]["turn_detection"] = Value::Null;
    p.consume(json!({"type":"session.created","session":session}))
        .unwrap();
    p.consume(json!({"type":"session.updated","session":session}))
        .unwrap();
    assert!(p.ready);
    assert!(p.asr.is_local());
    p
}

fn commit(asr: &mut Asr, start: u64, end: u64) -> Value {
    let mut at = start;
    while at < end {
        let next = (at + 15).min(end);
        asr.append_local(at, next).unwrap();
        at = next;
    }
    asr.commit_local(CommitBoundary {
        start_frame: start,
        end_frame: end,
        reason: CommitReason::Silence,
    })
    .unwrap()
}

fn receipt(id: &str, previous: Option<&str>) -> Value {
    json!({"type":"input_audio_buffer.committed","item_id":id,"previous_item_id":previous})
}

fn delta(id: &str, text: &str) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.delta","item_id":id,"content_index":0,"delta":text})
}

fn completed(id: &str, text: &str) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":text,"usage":{"type":"duration","seconds":0.1}})
}

#[test]
fn manual_startup_is_explicit_and_never_accepts_server_vad_fallback() {
    let request = protocol::start(Route::Transcription, Language::Ja);
    assert_eq!(
        request["session"]["audio"]["input"]["transcription"]["model"],
        "gpt-live-transcribe"
    );
    assert!(
        request["session"]["audio"]["input"]
            .as_object()
            .unwrap()
            .contains_key("turn_detection")
    );
    assert!(request["session"]["audio"]["input"]["turn_detection"].is_null());
    for unexpected in [
        json!({"type":"server_vad"}),
        json!({"type":"semantic_vad"}),
        json!(false),
    ] {
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        p.consume(json!({"type":"session.created","session":resolved(Route::Transcription)}))
            .unwrap();
        let mut configured = resolved(Route::Transcription);
        configured["audio"]["input"]["turn_detection"] = unexpected;
        assert!(
            p.consume(json!({"type":"session.updated","session":configured}))
                .is_err()
        );
        assert!(!p.ready);
    }
}

#[test]
fn receipt_fifo_and_out_of_order_finals_preserve_cumulative_source_items() {
    let mut asr = Asr::local();
    assert_eq!(
        commit(&mut asr, 50, 200)["type"],
        "input_audio_buffer.commit"
    );
    commit(&mut asr, 250, 300);
    assert!(!asr.complete());
    assert!(asr.deadline().is_some());
    assert!(
        asr.consume(receipt("source_a", None), 200)
            .unwrap()
            .is_empty()
    );
    assert!(
        asr.consume(receipt("source_b", Some("source_a")), 200)
            .unwrap()
            .is_empty()
    );
    let a = asr.consume(delta("source_a", "Asha and "), 200).unwrap();
    assert!(
        matches!(&a[..], [StreamEvent::Transcript { text, final_fragment:false, item:Some(item), .. }] if text == "Asha and " && item.sequence == 1 && item.revision == 1)
    );
    let a = asr.consume(delta("source_a", "Ken"), 200).unwrap();
    assert!(
        matches!(&a[..], [StreamEvent::Transcript { text, final_fragment:false, item:Some(item), .. }] if text == "Asha and Ken" && item.sequence == 1 && item.revision == 2)
    );
    asr.consume(delta("source_b", "draft amount"), 200).unwrap();
    assert!(
        asr.consume(completed("source_b", "The amount is twenty."), 200)
            .unwrap()
            .is_empty()
    );
    assert!(!asr.complete());
    let finals = asr
        .consume(completed("source_a", "Asha and Ken agreed."), 200)
        .unwrap();
    assert_eq!(finals.len(), 2);
    assert!(
        matches!(&finals[0], StreamEvent::Transcript { text, final_fragment:true, item:Some(item), .. } if text == "Asha and Ken agreed." && item.id == "source_a" && item.sequence == 1 && item.revision == 3)
    );
    assert!(
        matches!(&finals[1], StreamEvent::Transcript { text, final_fragment:true, item:Some(item), .. } if text == "The amount is twenty." && item.id == "source_b" && item.sequence == 2 && item.revision == 2)
    );
    assert!(asr.complete());

    commit(&mut asr, 350, 360);
    asr.consume(receipt("source_c", Some("source_b")), 210)
        .unwrap();
    asr.consume(delta("source_c", "tentative sound"), 210)
        .unwrap();
    let tombstone = asr.consume(completed("source_c", ""), 210).unwrap();
    assert!(
        matches!(&tombstone[..], [StreamEvent::Transcript { text, final_fragment:true, item:Some(item), .. }] if text.is_empty() && item.id == "source_c" && item.sequence == 3 && item.revision == 2)
    );
    assert!(asr.complete());
    assert!(
        asr.consume(delta("source_b", "late private text"), 210)
            .is_err()
    );
}

#[test]
fn unsolicited_duplicate_and_forked_commit_receipts_are_rejected() {
    let mut empty = Asr::local();
    assert!(empty.consume(receipt("unsolicited", None), 0).is_err());
    for second in [
        receipt("first", Some("first")),
        receipt("second", None),
        receipt("second", Some("foreign")),
        receipt("unsafe/id", Some("first")),
    ] {
        let mut asr = Asr::local();
        commit(&mut asr, 0, 20);
        commit(&mut asr, 40, 60);
        asr.consume(receipt("first", None), 40).unwrap();
        assert!(asr.consume(second, 40).is_err());
    }
    let mut asr = Asr::local();
    commit(&mut asr, 0, 20);
    asr.consume(receipt("sealed", None), 20).unwrap();
    asr.consume(completed("sealed", "final"), 20).unwrap();
    commit(&mut asr, 40, 60);
    assert!(asr.consume(receipt("sealed", Some("sealed")), 40).is_err());
}

#[test]
fn local_boundaries_require_exact_nonoverlapping_appended_ranges() {
    for (start, end) in [(0, 0), (20, 19)] {
        assert!(Asr::local().append_local(start, end).is_err());
    }
    for (start, end) in [(11, 40), (10, 39), (10, 41)] {
        let mut asr = Asr::local();
        asr.append_local(10, 20).unwrap();
        asr.append_local(20, 30).unwrap();
        asr.append_local(30, 40).unwrap();
        assert!(
            asr.commit_local(CommitBoundary {
                start_frame: start,
                end_frame: end,
                reason: CommitReason::Silence
            })
            .is_err()
        );
    }
    let mut asr = Asr::local();
    assert!(
        asr.commit_local(CommitBoundary {
            start_frame: 0,
            end_frame: 20,
            reason: CommitReason::FinishInput
        })
        .is_err()
    );
    asr.append_local(10, 20).unwrap();
    assert!(
        !asr.complete(),
        "appended but uncommitted speech is not drained"
    );
    assert!(asr.deadline().is_some());
    assert!(
        asr.append_local(21, 30).is_err(),
        "a hole within an utterance must not disappear"
    );
}

#[test]
fn missing_receipts_and_finals_share_one_bounded_backlog() {
    let mut asr = Asr::local();
    for n in 0..8 {
        commit(&mut asr, n * 20, n * 20 + 10);
    }
    assert!(!asr.complete());
    assert!(asr.deadline().is_some());
    // Receipt acknowledgement does not release capacity: its final is still owed.
    asr.consume(receipt("pending_final", None), 80).unwrap();
    assert!(asr.append_local(200, 210).is_err());
}

#[test]
fn delayed_receipts_and_deltas_do_not_extend_original_commit_deadline() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 20);
    let original = asr.deadline().unwrap();
    let remaining = original.saturating_duration_since(Instant::now());
    assert!(remaining > Duration::from_secs(44) && remaining <= Duration::from_secs(45));
    asr.consume(receipt("slow", None), 20).unwrap();
    assert_eq!(asr.deadline(), Some(original));
    asr.consume(delta("slow", "partial"), 20).unwrap();
    assert_eq!(
        asr.deadline(),
        Some(original),
        "partial updates cannot renew pending-final authority"
    );
}

#[test]
fn early_model_text_is_only_a_transient_draft_until_matching_fifo_receipt_binds_its_item() {
    let mut asr = Asr::local();
    asr.append_local(10, 20).unwrap();
    let first = asr.consume(delta("early", "unbound "), 10).unwrap();
    assert!(
        matches!(&first[..], [StreamEvent::SourceDraft{item_id,revision:1,text,retired:false}] if item_id=="early" && text=="unbound ")
    );
    let second = asr.consume(delta("early", "draft"), 10).unwrap();
    assert!(
        matches!(&second[..], [StreamEvent::SourceDraft{item_id,revision:2,text,retired:false}] if item_id=="early" && text=="unbound draft")
    );
    asr.commit_local(CommitBoundary {
        start_frame: 10,
        end_frame: 20,
        reason: CommitReason::FinishInput,
    })
    .unwrap();
    let out = asr.consume(receipt("early", None), 10).unwrap();
    assert!(
        matches!(&out[..], [StreamEvent::SourceDraft { item_id, revision:2, text:empty, retired:true }, StreamEvent::Transcript { text, final_fragment:false, item:Some(item), .. }] if item_id=="early" && empty.is_empty() && text == "unbound draft" && item.id == "early" && item.sequence == 1 && item.revision == 1)
    );
    assert!(!asr.complete());
    let mut unbound = Asr::local();
    unbound.append_local(0, 10).unwrap();
    assert!(
        unbound
            .consume(completed("unknown", "premature final"), 10)
            .is_err()
    );
}

#[test]
fn deterministic_segment_to_receipt_pipeline_preserves_original_frame_ranges() {
    let mut segmenter = Segmenter::new();
    let mut asr = Asr::local();
    let mut selected = Vec::new();
    let mut boundary = None;
    for n in 0u16..165 {
        let bytes: Vec<u8> = std::iter::repeat_n(n.to_le_bytes(), 320)
            .flatten()
            .collect();
        let segment = segmenter.push(&bytes, (40..140).contains(&n)).unwrap();
        if !segment.append.is_empty() {
            assert_eq!(
                segment.append.len() / 640,
                (segment.end_frame - segment.start_frame) as usize
            );
            selected.extend(
                segment
                    .append
                    .chunks_exact(640)
                    .map(|frame| u16::from_le_bytes([frame[0], frame[1]])),
            );
            asr.append_local(segment.start_frame, segment.end_frame)
                .unwrap();
        }
        if let Some(commit_boundary) = segment.commit {
            assert!(boundary.replace(commit_boundary).is_none());
            asr.commit_local(commit_boundary).unwrap();
        }
    }
    let boundary = boundary.unwrap();
    assert_eq!((boundary.start_frame, boundary.end_frame), (27, 165));
    assert_eq!(
        selected,
        (27u16..165).collect::<Vec<_>>(),
        "no repeated preroll, missing active frame or fabricated padding"
    );
    assert!(segmenter.finish().unwrap().commit.is_none());
    assert!(
        !asr.complete(),
        "local silence boundary is not a provider receipt"
    );
    asr.consume(receipt("ordered", None), selected.len() as u64)
        .unwrap();
    assert!(!asr.complete());
    asr.consume(
        completed("ordered", "synthetic receipt"),
        selected.len() as u64,
    )
    .unwrap();
    assert!(asr.complete());
}

#[test]
fn actual_local_detector_finishes_three_synthetic_signals_without_added_silence() {
    for variant in 0..3 {
        let pcm = crate::test_audio::signal(180, variant);
        let mut vad = LocalVad::new().unwrap();
        let mut appended = 0;
        let mut previous_end = 0;
        let mut boundaries = Vec::new();
        for frame in pcm.chunks_exact(640) {
            let segment = vad.push(frame).unwrap();
            if !segment.append.is_empty() {
                assert!(segment.start_frame >= previous_end);
                assert!(segment.append.len() <= 9600);
                assert_eq!(
                    segment.append,
                    pcm[segment.start_frame as usize * 640..segment.end_frame as usize * 640]
                );
                previous_end = segment.end_frame;
                appended += segment.append.len() / 640;
            }
            if let Some(boundary) = segment.commit {
                boundaries.push(boundary);
            }
        }
        let finish = vad.finish().unwrap();
        assert!(finish.append.is_empty());
        if let Some(boundary) = finish.commit {
            boundaries.push(boundary);
        }
        assert!(appended > 0 && appended <= pcm.len() / 640);
        assert!(!boundaries.is_empty());
        assert!(
            boundaries
                .iter()
                .all(|b| b.end_frame > b.start_frame && b.end_frame - b.start_frame < 6000)
        );
        assert!(
            boundaries
                .windows(2)
                .all(|b| b[0].end_frame <= b[1].start_frame)
        );
        println!(
            "signal {variant}: captured_frames={} selected_frames={appended} boundaries={boundaries:?}",
            pcm.len() / 640
        );
    }
}

#[tokio::test]
async fn prelatched_stop_suppresses_queued_audio_and_finish_without_commit() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    input.send(StreamInput::Pcm16(vec![0; 640])).await.unwrap();
    input.send(StreamInput::FinishInput).await.unwrap();
    let (events, mut output) = tokio::sync::mpsc::channel(8);
    let (_stop, mut stopped) = watch::channel(true);
    let mut global = backend.shared.stop.subscribe();
    let mut p = manual_ready();
    transport::drive(
        &backend.shared,
        &mut client,
        &mut p,
        input_rx,
        &events,
        &mut stopped,
        &mut global,
        Instant::now() + Duration::from_secs(20),
    )
    .await
    .unwrap();
    assert_eq!(p.frames, 0);
    assert!(p.complete_input());
    assert!(output.try_recv().is_err());
    assert!(
        tokio::time::timeout(Duration::from_millis(40), server.next())
            .await
            .is_err(),
        "Stop emitted upstream data"
    );
}

#[tokio::test]
async fn finish_waits_for_missing_commit_receipt_and_reports_observation_cutoff() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(8);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = manual_ready();
        p.frames = 20;
        commit(&mut p.asr, 0, 20);
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(12),
        )
        .await
        .unwrap();
        assert!(!p.complete_input());
    });
    input.send(StreamInput::FinishInput).await.unwrap();
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), output.recv())
            .await
            .is_err(),
        "a locally sent commit cannot stand in for its missing receipt"
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Drained {
            completion: DrainCompletion::ObservationWindowEnded
        })
    ));
    task.await.unwrap();
    // No synthetic final or additional positive flush was written by FinishInput.
    assert!(!matches!(server.next().await, Some(Ok(Message::Text(_)))));
}

#[tokio::test]
async fn actual_silent_capture_keeps_source_pacing_without_upstream_empty_commits() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(8);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = manual_ready();
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(20),
        )
        .await
        .unwrap();
        assert_eq!(p.captured_frames, 110);
        assert_eq!(
            p.frames, 0,
            "suppressed silence is not transmitted provider audio"
        );
        assert!(p.complete_input());
    });
    // Cross the2s source-gap window using real, paced20ms capture frames.
    for _ in 0..110 {
        input.send(StreamInput::Pcm16(vec![0; 640])).await.unwrap();
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    input.send(StreamInput::FinishInput).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    task.await.unwrap();
    assert!(
        !matches!(server.next().await, Some(Ok(Message::Text(_)))),
        "silence must not produce empty upstream appends/commits"
    );
}

#[tokio::test]
async fn actual_receipts_and_finals_are_required_before_successful_drain() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(8);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = manual_ready();
        p.frames = 50;
        commit(&mut p.asr, 20, 70);
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(20),
        )
        .await
        .unwrap();
        assert!(p.complete_input());
    });
    input.send(StreamInput::FinishInput).await.unwrap();
    server
        .send(Message::Text(receipt("manual_a", None).to_string().into()))
        .await
        .unwrap();
    server
        .send(Message::Text(
            delta("manual_a", "unfinished").to_string().into(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await, Some(StreamEvent::Transcript { final_fragment:false, item:Some(item), .. }) if item.revision==1)
    );
    assert!(
        tokio::time::timeout(Duration::from_millis(1100), output.recv())
            .await
            .is_err(),
        "an interim receipt is not final completion"
    );
    server
        .send(Message::Text(
            completed("manual_a", "finished synthetic text")
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(
        matches!(output.recv().await, Some(StreamEvent::Transcript { final_fragment:true, item:Some(item), .. }) if item.revision==2)
    );
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(2), output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    task.await.unwrap();
}
