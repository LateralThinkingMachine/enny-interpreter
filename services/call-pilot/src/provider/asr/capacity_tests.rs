//! Accelerated, key-free resource and source-order checks. Detector activity is
//! scripted; these are not a real-time provider soak or language acceptance.
use super::*;
use crate::provider::local_vad::{BoundaryError, Segmenter};
use serde_json::json;

fn append(asr: &mut Asr, start: u64, end: u64) {
    let mut at = start;
    while at < end {
        let next = (at + 15).min(end);
        asr.append_local(at, next).unwrap();
        at = next;
    }
}

fn commit(asr: &mut Asr, start: u64, end: u64) {
    append(asr, start, end);
    asr.commit_local(CommitBoundary {
        start_frame: start,
        end_frame: end,
        reason: CommitReason::Silence,
    })
    .unwrap();
}

fn receipt(id: &str, previous: Option<&str>) -> Value {
    json!({"type":"input_audio_buffer.committed","item_id":id,"previous_item_id":previous})
}

fn delta(id: &str, text: &str) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.delta","item_id":id,"content_index":0,"delta":text})
}

fn completed(id: &str, text: &str, seconds: f64) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":text,"usage":{"type":"duration","seconds":seconds}})
}

#[test]
fn nine_hundred_seconds_of_bounded_source_utterances_preserve_order_and_capacity() {
    let mut segmenter = Segmenter::new();
    let mut asr = Asr::local();
    let mut selected_frames = 0;
    let mut prior_end = 0;
    let mut previous: Option<String> = None;
    let mut finals = 0;
    // Each synthetic four-second cycle contains three seconds of detector-
    // positive speech and one second of negative tail. Boundaries come from
    // the production segmenter, never a periodic commit or fabricated final.
    for frame in 0..45_000_u64 {
        let segment = segmenter.push(&[0; 640], frame % 200 < 150).unwrap();
        if !segment.append.is_empty() {
            assert!(segment.start_frame >= prior_end);
            assert_eq!(
                segment.append.len() as u64 / 640,
                segment.end_frame - segment.start_frame
            );
            asr.append_local(segment.start_frame, segment.end_frame)
                .unwrap();
            selected_frames += segment.end_frame - segment.start_frame;
            prior_end = segment.end_frame;
        }
        if let Some(boundary) = segment.commit {
            assert_eq!(boundary.reason, CommitReason::Silence);
            asr.commit_local(boundary).unwrap();
            let id = format!("source_{}", finals + 1);
            assert!(
                asr.consume(receipt(&id, previous.as_deref()), selected_frames)
                    .unwrap()
                    .is_empty()
            );
            let expected = format!("架空の項目 {}", finals + 1);
            let draft = asr.consume(delta(&id, &expected), selected_frames).unwrap();
            assert!(matches!(draft.as_slice(), [StreamEvent::Transcript {
                final_fragment: false, item: Some(item), ..
            }] if item.sequence == finals + 1));
            let seconds = (boundary.end_frame - boundary.start_frame) as f64 / 50.0;
            let output = asr
                .consume(completed(&id, &expected, seconds), selected_frames)
                .unwrap();
            finals += 1;
            assert!(matches!(output.as_slice(), [StreamEvent::Transcript {
                final_fragment: true, text, item: Some(item), ..
            }] if text == &expected && item.sequence == finals && item.revision == 2));
            assert!(asr.complete());
            assert!(asr.items.is_empty());
            assert!(asr.local.as_ref().unwrap().pending.is_empty());
            previous = Some(id);
        }
        asr.check(selected_frames).unwrap();
    }
    assert!(segmenter.finish().unwrap().commit.is_none());
    assert_eq!(finals, 225);
    assert_eq!(asr.seen.len(), 225);
    assert_eq!(asr.local.as_ref().unwrap().ordinal, 225);
    assert!(asr.complete());
    assert!(asr.deadline().is_none());
    assert!(asr.usage > 120.0 && asr.usage <= 900.0);
    assert!((asr.usage - selected_frames as f64 / 50.0).abs() < 0.000_001);
}

#[test]
fn continuous_speech_guard_stays_incomplete_and_is_not_a_spending_failure() {
    let mut segmenter = Segmenter::new();
    let mut asr = Asr::local();
    for frame in 0..6000 {
        let result = segmenter.push(&[0; 640], true);
        if frame == 5999 {
            assert_eq!(result, Err(BoundaryError::TurnLimit));
            break;
        }
        let segment = result.unwrap();
        assert!(segment.commit.is_none());
        if !segment.append.is_empty() {
            asr.append_local(segment.start_frame, segment.end_frame)
                .unwrap();
        }
    }
    assert_eq!(
        asr.append_local(5999, 6000),
        Err(ProviderError::SpeechBoundaryLimit)
    );
    assert!(!asr.complete());
    assert_eq!(asr.local.as_ref().unwrap().ordinal, 0);
    assert!(asr.items.is_empty());
    assert!(asr.local.as_ref().unwrap().pending.is_empty());
    assert!(!segmenter.active());
    assert_eq!(segmenter.finish(), Err(BoundaryError::Closed));
    asr.local.as_mut().unwrap().active.as_mut().unwrap().entered = Instant::now() - MAX_TURN;
    assert_eq!(asr.check(5999), Err(ProviderError::SpeechBoundaryLimit));
}

#[test]
fn missing_source_responses_retain_their_original_deadline_and_capacity() {
    let mut asr = Asr::local();
    for ordinal in 0..8 {
        commit(&mut asr, ordinal * 20, ordinal * 20 + 10);
    }
    assert_eq!(
        asr.append_local(160, 170),
        Err(ProviderError::SourceLedgerLimit)
    );
    let original = asr.deadline();
    asr.consume(receipt("first", None), 80).unwrap();
    asr.consume(delta("first", "tentative"), 80).unwrap();
    assert_eq!(asr.deadline(), original);
    assert_eq!(
        asr.append_local(160, 170),
        Err(ProviderError::SourceLedgerLimit)
    );
    asr.items.front_mut().unwrap().pending_since = Some(Instant::now() - MAX_PENDING_AGE);
    assert_eq!(asr.check(80), Err(ProviderError::SourceResponseDeadline));
    assert!(!asr.complete());

    let mut missing_receipt = Asr::local();
    commit(&mut missing_receipt, 0, 10);
    missing_receipt.local.as_mut().unwrap().pending[0].sent = Instant::now() - MAX_PENDING_AGE;
    assert_eq!(
        missing_receipt.check(10),
        Err(ProviderError::SourceResponseDeadline)
    );
    assert!(matches!(
        missing_receipt.consume(receipt("late", None), 10),
        Err(ProviderError::SourceResponseDeadline)
    ));
    assert!(!missing_receipt.complete());
}

#[test]
fn exact_lifetime_item_limit_is_not_a_short_meeting_or_dollar_limit() {
    let mut asr = Asr::local();
    let mut previous: Option<String> = None;
    for ordinal in 1..=4096 {
        let start = (ordinal - 1) * 10;
        commit(&mut asr, start, start + 10);
        let id = format!("source_{ordinal}");
        asr.consume(receipt(&id, previous.as_deref()), ordinal * 10)
            .unwrap();
        asr.consume(completed(&id, "fictional", 0.2), ordinal * 10)
            .unwrap();
        previous = Some(id);
        assert!(asr.complete());
    }
    assert_eq!(asr.seen.len(), 4096);
    assert_eq!(
        asr.append_local(40960, 40970),
        Err(ProviderError::SourceLedgerLimit)
    );
    assert!(
        asr.complete(),
        "refused input cannot create a new source item"
    );
}

#[test]
fn one_item_unicode_and_revision_limits_are_separate_from_shared_text_allowance() {
    let maximum = "🗂".repeat(4096);
    assert_eq!(maximum.len(), 16_384);
    let mut early = Asr::local();
    append(&mut early, 0, 10);
    assert!(
        matches!(early.consume(delta("early", &maximum),10).unwrap().as_slice(),
        [StreamEvent::SourceDraft {revision:1,text,retired:false,..}] if text==&maximum)
    );
    assert!(matches!(
        early.consume(delta("early", "界"), 10),
        Err(ProviderError::TranscriptItemLimit)
    ));
    assert!(!early.complete());

    let mut bound = Asr::local();
    commit(&mut bound, 0, 10);
    bound.consume(receipt("bound", None), 10).unwrap();
    bound.consume(delta("bound", &maximum), 10).unwrap();
    assert!(matches!(
        bound.consume(delta("bound", "a"), 10),
        Err(ProviderError::TranscriptItemLimit)
    ));
    bound.items.front_mut().unwrap().partial.clear();
    bound.items.front_mut().unwrap().revision = 4096;
    assert!(matches!(
        bound.consume(delta("bound", "a"), 10),
        Err(ProviderError::TranscriptItemLimit)
    ));
    assert!(matches!(
        bounded_text(&json!({"text":"界".repeat(4097)}), "text"),
        Err(ProviderError::TranscriptItemLimit)
    ));
    assert_eq!(
        bounded_text(&json!({"text":"bad\u{0000}text"}), "text"),
        Err(ProviderError::Protocol)
    );
}

#[test]
fn inconsistent_source_usage_is_identified_without_changing_its_bound() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 10);
    asr.consume(receipt("source", None), 10).unwrap();
    assert!(matches!(
        asr.consume(completed("source", "fictional", 1.201), 10),
        Err(ProviderError::AsrUsageMismatch {
            submitted_ms: 200,
            reported_ms: Some(1201),
            allowed_ms: 1200,
        })
    ));
    assert_eq!(asr.usage, 0.0);
    assert!(!asr.complete());
    assert!(asr.items.front().unwrap().final_text.is_none());
}
