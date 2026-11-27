use super::*;
use serde_json::json;

fn started() -> Asr {
    let mut asr = Asr::default();
    asr.consume(json!({"type":"input_audio_buffer.speech_started","item_id":"long_presentation","audio_start_ms":0}), 0).unwrap();
    asr
}

#[test]
fn continuous_presentation_crosses_thirty_seconds_without_forced_final() {
    let mut asr = started();
    asr.items.front_mut().unwrap().entered = Instant::now() - Duration::from_secs(90);
    // 90 seconds of input can remain a single natural utterance. Names/numbers
    // are keyed interim text; no timer invents a completed source record.
    let output = asr.consume(json!({"type":"conversation.item.input_audio_transcription.delta","item_id":"long_presentation","content_index":0,"delta":"Asha and Ken: 12,000 yen, September 13, 20:00 JST."}), 4500).unwrap();
    assert!(
        matches!(&output[0], StreamEvent::Transcript { final_fragment: false, text, .. } if text.contains("12,000"))
    );
    assert!(!asr.complete());
    assert_eq!(asr.manual_commits, 0);
    assert_eq!(MAX_TURN, Duration::from_secs(120));
    assert!(asr.check(5999).is_ok());
    assert_eq!(asr.check(6000), Err(ProviderError::SpeechBoundaryLimit));
}

#[test]
fn final_wait_starts_after_long_speech_and_commit_cannot_extend_it() {
    let mut asr = started();
    asr.items.front_mut().unwrap().entered = Instant::now() - Duration::from_secs(90);
    let before = Instant::now();
    asr.consume(json!({"type":"input_audio_buffer.speech_stopped","item_id":"long_presentation","audio_end_ms":90000}),4500).unwrap();
    let pending = asr.deadline().unwrap();
    assert!(pending >= before + Duration::from_secs(45));
    asr.consume(json!({"type":"input_audio_buffer.committed","item_id":"long_presentation","previous_item_id":null}),4500).unwrap();
    assert_eq!(asr.deadline(), Some(pending));
    let output = asr.consume(json!({"type":"conversation.item.input_audio_transcription.completed","item_id":"long_presentation","content_index":0,"transcript":"Asha and Ken meet at20:00JST. Budget12,000yen.","usage":{"type":"duration","seconds":90}}),4500).unwrap();
    assert!(matches!(
        &output[0],
        StreamEvent::Transcript {
            final_fragment: true,
            ..
        }
    ));
    assert!(asr.complete());
    assert_eq!(asr.usage, 90.0);
}

#[test]
fn missing_final_and_stalled_input_still_have_finite_deadlines() {
    let mut asr = started();
    asr.items.front_mut().unwrap().entered = Instant::now() - Duration::from_secs(121);
    assert_eq!(asr.check(1), Err(ProviderError::SpeechBoundaryLimit));
    let mut asr = started();
    asr.consume(json!({"type":"input_audio_buffer.speech_stopped","item_id":"long_presentation","audio_end_ms":2000}),100).unwrap();
    asr.items.front_mut().unwrap().pending_since = Some(Instant::now() - Duration::from_secs(46));
    assert_eq!(asr.check(100), Err(ProviderError::SourceResponseDeadline));
}
