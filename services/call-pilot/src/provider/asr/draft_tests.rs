//! A draft can be displayed promptly without acquiring a source ordinal or
//! relaxing completion/usage checks. All input and provider events are synthetic.
use super::*;
use serde_json::json;

fn delta(id: &str, text: &str) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.delta","item_id":id,"content_index":0,"delta":text})
}
fn commit(asr: &mut Asr, start: u64, end: u64) {
    let mut frame = start;
    while frame < end {
        let next = (frame + 15).min(end);
        asr.append_local(frame, next).unwrap();
        frame = next;
    }
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
fn final_item(id: &str, text: &str) -> Value {
    json!({"type":"conversation.item.input_audio_transcription.completed","item_id":id,"content_index":0,"transcript":text,"usage":{"type":"duration","seconds":0.1}})
}

#[test]
fn immediate_unicode_draft_keeps_receipt_order_and_charges_only_received_text() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 20);
    asr.append_local(30, 40).unwrap();
    let deadline = asr.deadline().unwrap();
    // Arrival order is deliberately the reverse of eventual source order.
    for (id, text) in [
        ("second", "金額は"),
        ("first", "Asha "),
        ("second", "12,000円"),
        ("first", "और Ken"),
    ] {
        let out = asr.consume(delta(id, text), 30).unwrap();
        assert_eq!(asr.text_charge, text.chars().count());
        assert!(
            matches!(&out[..],[StreamEvent::SourceDraft{item_id,retired:false,..}] if item_id==id)
        );
        assert!(asr.items.is_empty());
        assert!(asr.seen.is_empty());
        assert_eq!(asr.local.as_ref().unwrap().ordinal, 1);
        assert_eq!(asr.deadline(), Some(deadline));
    }
    asr.commit_local(CommitBoundary {
        start_frame: 30,
        end_frame: 40,
        reason: CommitReason::Silence,
    })
    .unwrap();
    for (id, prior, sequence, text) in [
        ("first", None, 1, "Asha और Ken"),
        ("second", Some("first"), 2, "金額は12,000円"),
    ] {
        let out = asr.consume(receipt(id, prior), 30).unwrap();
        assert_eq!(
            asr.text_charge, 0,
            "draft retirement and canonical republishing must not charge text again"
        );
        assert!(
            matches!(&out[..],[StreamEvent::SourceDraft{item_id,revision:2,text:removed,retired:true},StreamEvent::Transcript{text:bound,final_fragment:false,item:Some(item),..}] if item_id==id && removed.is_empty() && bound==text && item.sequence==sequence && item.revision==1)
        );
    }
    assert!(asr.local.as_ref().unwrap().early.is_empty());
    assert!(
        asr.consume(final_item("second", "金額は12,000円です。"), 30)
            .unwrap()
            .is_empty()
    );
    let finals = asr
        .consume(final_item("first", "Asha और Ken agreed."), 30)
        .unwrap();
    assert!(
        matches!(&finals[..],[StreamEvent::Transcript{final_fragment:true,item:Some(first),..},StreamEvent::Transcript{final_fragment:true,item:Some(second),..}] if first.sequence==1 && second.sequence==2)
    );
    assert!(asr.complete());
    assert!(
        asr.consume(delta("first", "late resurrection"), 30)
            .is_err()
    );
}

#[test]
fn draft_updates_cannot_renew_deadlines_bind_unknown_finals_or_hide_usage_mismatch() {
    let mut asr = Asr::local();
    asr.append_local(0, 10).unwrap();
    asr.consume(delta("unbound", "draft"), 10).unwrap();
    let expires = asr.local.as_ref().unwrap().early["unbound"].expires;
    asr.consume(delta("unbound", " updates"), 10).unwrap();
    assert_eq!(
        asr.local.as_ref().unwrap().early["unbound"].expires,
        expires
    );
    assert!(matches!(
        asr.consume(final_item("unbound", "premature"), 10),
        Err(ProviderError::Protocol)
    ));
    asr.commit_local(CommitBoundary {
        start_frame: 0,
        end_frame: 10,
        reason: CommitReason::FinishInput,
    })
    .unwrap();
    let deadline = asr.deadline();
    asr.consume(delta("unbound", " while waiting"), 10).unwrap();
    assert_eq!(asr.deadline(), deadline);
    asr.consume(receipt("unbound", None), 10).unwrap();
    let mut bad = final_item("unbound", "not accepted");
    bad["usage"]["seconds"] = json!(2);
    assert!(matches!(
        asr.consume(bad, 10),
        Err(ProviderError::AsrUsageMismatch {
            submitted_ms: 200,
            allowed_ms: 1200,
            ..
        })
    ));
    assert!(!asr.complete());
    assert_eq!(asr.usage, 0.0);
}

#[test]
fn empty_deltas_do_not_allocate_ids_or_revisions_and_existing_bounds_stay_exact() {
    let mut asr = Asr::local();
    asr.append_local(0, 10).unwrap();
    for n in 0..100 {
        assert!(
            asr.consume(delta(&format!("empty_{n}"), ""), 10)
                .unwrap()
                .is_empty()
        );
        assert_eq!(asr.text_charge, 0);
    }
    assert!(asr.local.as_ref().unwrap().early.is_empty());
    for n in 0..8 {
        asr.consume(delta(&format!("draft_{n}"), "a"), 10).unwrap();
    }
    assert!(matches!(
        asr.consume(delta("ninth", "a"), 10),
        Err(ProviderError::SourceLedgerLimit)
    ));
    let mut revisions = Asr::local();
    revisions.append_local(0, 10).unwrap();
    for n in 1..=4096 {
        assert!(
            matches!(revisions.consume(delta("max","a"),10).unwrap().as_slice(),[StreamEvent::SourceDraft{revision,text,retired:false,..}] if *revision==n && text.len()==n as usize)
        );
    }
    assert!(revisions.consume(delta("max", ""), 10).unwrap().is_empty());
    revisions
        .commit_local(CommitBoundary {
            start_frame: 0,
            end_frame: 10,
            reason: CommitReason::FinishInput,
        })
        .unwrap();
    let retired = revisions.consume(receipt("max", None), 10).unwrap();
    assert!(
        matches!(&retired[0],StreamEvent::SourceDraft{revision:4096,text,retired:true,..} if text.is_empty())
    );
    assert_eq!(revisions.text_charge, 0);
    let final_out = revisions
        .consume(final_item("max", "accepted corrected short final"), 10)
        .unwrap();
    assert!(
        matches!(&final_out[..],[StreamEvent::Transcript{final_fragment:true,item:Some(item),..}] if item.revision==2)
    );
    assert!(revisions.complete());
}

#[test]
fn expired_orphan_draft_is_incomplete_and_cannot_be_mistaken_for_a_final() {
    let mut asr = Asr::local();
    commit(&mut asr, 0, 10);
    asr.consume(delta("unmatched", "maybe another item"), 10)
        .unwrap();
    // A different genuine receipt never silently binds an orphan draft by order.
    assert!(asr.consume(receipt("actual", None), 10).unwrap().is_empty());
    asr.consume(final_item("actual", "confirmed item"), 10)
        .unwrap();
    assert!(!asr.complete());
    asr.local
        .as_mut()
        .unwrap()
        .early
        .get_mut("unmatched")
        .unwrap()
        .expires = Instant::now();
    assert!(matches!(
        asr.check(10),
        Err(ProviderError::SourceResponseDeadline)
    ));
}

#[test]
fn provisional_delivery_removes_withholding_without_moving_onset_or_tail_boundaries() {
    use crate::provider::local_vad::Segmenter;
    let mut segmenter = Segmenter::new();
    let mut asr = Asr::local();
    let mut selected = Vec::new();
    let mut draft_at = None;
    let mut bound_at = None;
    for frame in 0u16..165 {
        let bytes: Vec<u8> = std::iter::repeat_n(frame.to_le_bytes(), 320)
            .flatten()
            .collect();
        // Includes a 240ms within-speech pause. The unchanged detector-negative
        // tail policy must keep this word group together.
        let speech = (40..140).contains(&frame) && !(90..102).contains(&frame);
        let segment = segmenter.push(&bytes, speech).unwrap();
        if !segment.append.is_empty() {
            selected.extend(
                segment
                    .append
                    .chunks_exact(640)
                    .map(|pcm| u16::from_le_bytes([pcm[0], pcm[1]])),
            );
            asr.append_local(segment.start_frame, segment.end_frame)
                .unwrap();
        }
        if frame == 60 {
            let event = asr
                .consume(delta("live", "Asha: 12,000円"), selected.len() as u64)
                .unwrap();
            assert!(matches!(
                event.as_slice(),
                [StreamEvent::SourceDraft { retired: false, .. }]
            ));
            assert!(asr.seen.is_empty());
            assert!(asr.items.is_empty());
            draft_at = Some(u64::from(frame + 1) * 20);
        }
        if let Some(boundary) = segment.commit {
            assert_eq!((boundary.start_frame, boundary.end_frame), (27, 165));
            asr.commit_local(boundary).unwrap();
            let events = asr
                .consume(receipt("live", None), selected.len() as u64)
                .unwrap();
            assert!(matches!(
                events.as_slice(),
                [
                    StreamEvent::SourceDraft { retired: true, .. },
                    StreamEvent::Transcript {
                        final_fragment: false,
                        ..
                    }
                ]
            ));
            bound_at = Some(boundary.end_frame * 20);
        }
    }
    assert_eq!(selected, (27u16..165).collect::<Vec<_>>());
    assert_eq!(draft_at, Some(1220));
    assert_eq!(bound_at, Some(3300));
    assert_eq!(bound_at.unwrap() - draft_at.unwrap(), 2080);
    assert!(!asr.complete());
    let final_out = asr
        .consume(
            final_item("live", "Asha: 12,000円です。"),
            selected.len() as u64,
        )
        .unwrap();
    assert!(
        matches!(final_out.as_slice(),[StreamEvent::Transcript{final_fragment:true,text,..}] if text=="Asha: 12,000円です。")
    );
    assert!(asr.complete());
    println!(
        "deterministic source trace: provisional text at1220ms versus receipt-bound publication3300ms (2080ms removed local withholding); exact source range27..165 and500ms detector-negative tail unchanged; no provider speed/word-accuracy claim"
    );
}
