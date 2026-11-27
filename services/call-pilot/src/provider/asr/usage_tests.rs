//! Fixed synthetic usage receipts. No provider, credentials or audio devices.
use super::*;
use serde_json::json;

fn commit(asr: &mut Asr, start: u64, end: u64, id: &str, previous: Option<&str>) {
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
    .unwrap();
    asr.consume(
        json!({"type":"input_audio_buffer.committed","item_id":id,"previous_item_id":previous}),
        end,
    )
    .unwrap();
}

fn completed(id: &str, seconds: Value) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,
        "content_index":0,"transcript":"Fictional phrase.","usage":{"type":"duration","seconds":seconds}})
}

#[test]
fn repeated_short_turns_keep_independent_tolerance_and_reported_usage() {
    let mut asr = Asr::local();
    let mut previous = None;
    for n in 0..40_u64 {
        let id = format!("short_{n}");
        // 1.1 seconds selected from source ranges separated by long silence.
        commit(&mut asr, n * 200, n * 200 + 55, &id, previous.as_deref());
        let output = asr.consume(completed(&id, json!(2)), (n + 1) * 55).unwrap();
        assert!(matches!(output.as_slice(), [StreamEvent::Transcript {
            final_fragment: true, item: Some(item), ..
        }] if item.sequence == n as u32 + 1));
        assert_eq!(asr.usage, (n + 1) as f64 * 2.0);
        assert!(asr.complete());
        previous = Some(id);
    }
    assert_eq!(asr.usage, 80.0);
    assert_ne!(asr.usage, 40.0 * 55.0 / 50.0);
}

#[test]
fn out_of_order_finals_use_their_own_committed_lengths_and_preserve_source_order() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 55, "first", None);
    commit(&mut asr, 100, 110, "second", Some("first"));
    assert!(
        asr.consume(completed("second", json!(1)), 65)
            .unwrap()
            .is_empty()
    );
    assert_eq!(asr.usage, 1.0);
    let output = asr.consume(completed("first", json!(2)), 65).unwrap();
    assert_eq!(output.len(), 2);
    for (index, event) in output.iter().enumerate() {
        assert!(matches!(event, StreamEvent::Transcript {
            final_fragment:true, item:Some(item), ..
        } if item.sequence == index as u32 + 1
            && item.source_audio.as_ref().unwrap().admitted_audio_ms == [1100, 200][index]
            && item.source_audio.as_ref().unwrap().boundary == SourceAudioBoundary::DetectorSilence));
    }
    assert_eq!(asr.usage, 3.0);
    assert!(asr.complete());
}

#[test]
fn late_receipt_binds_exact_finish_duration_and_keeps_raw_empty_text_off_canonical_wire() {
    let mut asr = Asr::local();
    asr.append_local(300, 310).unwrap();
    asr.commit_local(CommitBoundary {
        start_frame: 300,
        end_frame: 310,
        reason: CommitReason::FinishInput,
    })
    .unwrap();
    // Finalizing without its receipt cannot establish source or audio evidence.
    assert!(asr.consume(completed("late", json!(0.2)), 10).is_err());
    // Later selected PCM belongs to a different item and cannot inflate this one.
    asr.append_local(400, 415).unwrap();
    asr.consume(
        json!({"type":"input_audio_buffer.committed","item_id":"late","previous_item_id":null}),
        25,
    )
    .unwrap();
    let mut final_event = completed("late", json!(0.2));
    final_event["transcript"] = json!(" \t ");
    let output = asr.consume(final_event, 25).unwrap();
    let [
        StreamEvent::Transcript {
            text,
            final_fragment: true,
            item: Some(item),
            ..
        },
    ] = output.as_slice()
    else {
        panic!("missing exact final")
    };
    assert_eq!(text, " \t ");
    let evidence = item.source_audio.as_ref().unwrap();
    assert_eq!(evidence.admitted_audio_ms, 200);
    assert_eq!(evidence.boundary, SourceAudioBoundary::ExplicitFinish);
    assert_eq!(
        evidence.detector,
        SourceAudioDetector::ManualLocalBoundariesV1
    );
    assert!(evidence.valid());
    assert_eq!(
        serde_json::to_value(item).unwrap(),
        json!({"id":"late","sequence":1,"revision":1})
    );
    assert_eq!(asr.usage, 0.2);
    assert!(asr.local.as_ref().unwrap().active.is_some());
}

#[test]
fn source_audio_evidence_rejects_noncanonical_enum_shapes_and_invalid_durations() {
    let exact = json!({"version":1,"admitted_audio_ms":200,"boundary":"detector_silence","detector":"manual_local_boundaries_v1"});
    let parsed: SourceAudioEvidence = serde_json::from_value(exact.clone()).unwrap();
    assert!(parsed.valid());
    assert_eq!(serde_json::to_value(parsed).unwrap(), exact);
    for (field, values) in [
        (
            "boundary",
            vec![
                json!({"detector_silence":null}),
                Value::Null,
                json!("guessed"),
            ],
        ),
        (
            "detector",
            vec![
                json!({"manual_local_boundaries_v1":null}),
                Value::Null,
                json!("guessed"),
            ],
        ),
    ] {
        for value in values {
            let mut invalid = exact.clone();
            invalid[field] = value;
            assert!(serde_json::from_value::<SourceAudioEvidence>(invalid).is_err());
        }
    }
    for (field, value) in [
        ("version", 2),
        ("admitted_audio_ms", 0),
        ("admitted_audio_ms", 101),
        ("admitted_audio_ms", 120_000),
    ] {
        let mut invalid = exact.clone();
        invalid[field] = json!(value);
        assert!(
            !serde_json::from_value::<SourceAudioEvidence>(invalid)
                .unwrap()
                .valid()
        );
    }
}

#[test]
fn gross_item_overclaim_cannot_borrow_later_appended_audio_credit() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 10, "short", None);
    // Later uncommitted speech adds 20 seconds to total appended audio.
    for start in (100..1100).step_by(10) {
        asr.append_local(start, start + 10).unwrap();
    }
    assert_eq!(
        asr.consume(completed("short", json!(20)), 1010).err(),
        Some(ProviderError::AsrUsageMismatch {
            submitted_ms: 200,
            reported_ms: Some(20_000),
            allowed_ms: 1200,
        })
    );
    assert_eq!(asr.usage, 0.0);
    assert!(asr.items.front().unwrap().final_text.is_none());
    assert!(asr.local.as_ref().unwrap().active.is_some());
    assert!(!asr.complete());
}

#[test]
fn local_tolerance_is_inclusive_but_does_not_accept_the_next_fraction() {
    for (reported, valid) in [(1.2, true), (1.200_001, false)] {
        let mut asr = Asr::local();
        commit(&mut asr, 0, 10, "short", None);
        let result = asr.consume(completed("short", json!(reported)), 10);
        assert_eq!(result.is_ok(), valid);
        assert_eq!(asr.usage, if valid { reported } else { 0.0 });
        assert_eq!(asr.complete(), valid);
    }
}

#[test]
fn invalid_and_duplicate_completions_cannot_charge_or_finalize_an_item() {
    for value in [
        json!(-0.1),
        Value::Null,
        json!("Infinity"),
        json!("NaN"),
        json!({}),
        json!(1e308),
    ] {
        let mut asr = Asr::local();
        commit(&mut asr, 0, 10, "short", None);
        assert!(asr.consume(completed("short", value), 10).is_err());
        assert_eq!(asr.usage, 0.0);
        assert!(asr.items.front().unwrap().final_text.is_none());
        assert!(!asr.complete());
    }
    // Non-finite JSON numbers cannot reach the parsed protocol value.
    assert!(serde_json::from_str::<Value>("1e999").is_err());
    let mut asr = Asr::local();
    commit(&mut asr, 0, 10, "short", None);
    asr.consume(completed("short", json!(1)), 10).unwrap();
    assert!(asr.consume(completed("short", json!(1)), 10).is_err());
    assert_eq!(asr.usage, 1.0);
    assert!(asr.complete());
}

#[test]
fn duplicate_out_of_order_final_cannot_consume_tolerance_twice() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 10, "first", None);
    commit(&mut asr, 10, 20, "second", Some("first"));
    assert!(
        asr.consume(completed("second", json!(1)), 20)
            .unwrap()
            .is_empty()
    );
    assert!(asr.consume(completed("second", json!(1)), 20).is_err());
    assert_eq!(asr.usage, 1.0);
    assert!(!asr.complete());
    assert_eq!(
        asr.consume(completed("first", json!(1)), 20).unwrap().len(),
        2
    );
    assert_eq!(asr.usage, 2.0);
}

#[test]
fn mismatch_metadata_keeps_exact_input_and_bounded_ceil_reported_milliseconds() {
    for (seconds, reported_ms) in [(1.200_001, Some(1201)), (1e308, None)] {
        let mut asr = Asr::local();
        commit(&mut asr, 0, 10, "short", None);
        assert_eq!(
            asr.consume(completed("short", json!(seconds)), 10).err(),
            Some(ProviderError::AsrUsageMismatch {
                submitted_ms: 200,
                reported_ms,
                allowed_ms: 1200,
            })
        );
        assert_eq!(asr.usage, 0.0);
        assert!(asr.items.front().unwrap().final_text.is_none());
    }
    assert_eq!(reported_millis(f64::INFINITY), None);
    assert_eq!(reported_millis(f64::NAN), None);
    assert_eq!(reported_millis(9_007_199_254_741.0), None);
    assert!(reported_millis(9_007_199_254_740.0).is_some());
}
