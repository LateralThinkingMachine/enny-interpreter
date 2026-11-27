//! Bounded local-commit item ledger, with retained server-VAD protocol traces
//! for offline regression. Source revisions are full snapshots, never
//! independent fragments or inferred speakers. Receipt completion is not coverage.
use super::local_vad::{CommitBoundary, CommitReason};
use crate::engine::{
    Provenance, ProviderError, SourceAudioBoundary, SourceAudioDetector, SourceAudioEvidence,
    StreamEvent, TranscriptItem,
};
use serde_json::Value;
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    time::{Duration, Instant},
};

// Local presentation guard, not a documented provider turn limit. Keep natural
// VAD boundaries even beyond 30 seconds; never force a time-cut final caption.
pub(super) const MAX_TURN: Duration = Duration::from_secs(120);
const MAX_PENDING_AGE: Duration = Duration::from_secs(45);
// Local receipt-validation tolerance per committed item. This is not a claim
// about the provider's billing rounding or measured account expenditure.
const ITEM_USAGE_TOLERANCE_MS: u32 = 1000;
struct Item {
    id: String,
    sequence: u32,
    revision: u32,
    partial: String,
    final_text: Option<String>,
    committed: bool,
    stopped: bool,
    start_ms: f64,
    entered: Instant,
    pending_since: Option<Instant>,
    committed_frames: Option<u64>,
    commit_reason: Option<CommitReason>,
}
struct LocalActive {
    start: u64,
    end: u64,
    entered: Instant,
}
struct LocalCommit {
    boundary: CommitBoundary,
    ordinal: u32,
    sent: Instant,
}
struct EarlyDraft {
    text: String,
    revision: u32,
    expires: Instant,
}
#[derive(Default)]
struct Local {
    active: Option<LocalActive>,
    pending: VecDeque<LocalCommit>,
    early: BTreeMap<String, EarlyDraft>,
    ordinal: u32,
    last_end: u64,
}
#[derive(Default)]
pub(super) struct Asr {
    local: Option<Local>,
    items: VecDeque<Item>,
    seen: BTreeSet<String>,
    active: Option<String>,
    last_committed: Option<String>,
    last_start: f64,
    manual_commits: u32,
    pub usage: f64,
    pub text_charge: usize,
}
fn field<'a>(v: &'a Value, key: &str) -> Result<&'a str, ProviderError> {
    v[key].as_str().ok_or(ProviderError::Protocol)
}
fn timestamp(v: &Value, key: &str, frames: u64) -> Result<f64, ProviderError> {
    v[key]
        .as_f64()
        .filter(|ms| {
            ms.is_finite() && *ms >= 0.0 && *ms <= frames.saturating_mul(20) as f64 + 100.0
        })
        .ok_or(ProviderError::Protocol)
}
fn bounded_text(v: &Value, key: &str) -> Result<String, ProviderError> {
    let text = field(v, key)?;
    if text.contains('\0') {
        return Err(ProviderError::Protocol);
    }
    if text.len() > 16384 || text.chars().count() > 4096 {
        return Err(ProviderError::TranscriptItemLimit);
    }
    Ok(text.to_owned())
}
fn reported_millis(seconds: f64) -> Option<u64> {
    let milliseconds = (seconds * 1000.0).ceil();
    (milliseconds.is_finite() && (0.0..=9_007_199_254_740_991.0).contains(&milliseconds))
        .then_some(milliseconds as u64)
}
impl Asr {
    pub fn local() -> Self {
        Self {
            local: Some(Local::default()),
            ..Self::default()
        }
    }
    pub fn is_local(&self) -> bool {
        self.local.is_some()
    }
    pub fn awaiting_local_finals(&self) -> bool {
        !self.items.is_empty() || self.local.as_ref().is_some_and(|l| !l.pending.is_empty())
    }
    pub fn append_local(&mut self, start: u64, end: u64) -> Result<(), ProviderError> {
        let local = self.local.as_mut().ok_or(ProviderError::Protocol)?;
        if end <= start || end - start > 15 {
            return Err(ProviderError::Protocol);
        }
        if let Some(active) = &mut local.active {
            if active.end != start {
                return Err(ProviderError::Protocol);
            }
            if end - active.start >= 6000 || active.entered.elapsed() >= MAX_TURN {
                return Err(ProviderError::SpeechBoundaryLimit);
            }
            active.end = end;
        } else {
            if start < local.last_end {
                return Err(ProviderError::Protocol);
            }
            if self.items.len() + local.pending.len() >= 8 || local.ordinal >= 4096 {
                return Err(ProviderError::SourceLedgerLimit);
            }
            local.active = Some(LocalActive {
                start,
                end,
                entered: Instant::now(),
            });
        }
        Ok(())
    }
    pub fn commit_local(&mut self, boundary: CommitBoundary) -> Result<Value, ProviderError> {
        let local = self.local.as_mut().ok_or(ProviderError::Protocol)?;
        let active = local.active.as_ref().ok_or(ProviderError::Protocol)?;
        if boundary.start_frame != active.start
            || boundary.end_frame != active.end
            || boundary.end_frame - boundary.start_frame < 5
        {
            return Err(ProviderError::Protocol);
        }
        if boundary.end_frame - boundary.start_frame >= 6000 || active.entered.elapsed() >= MAX_TURN
        {
            return Err(ProviderError::SpeechBoundaryLimit);
        }
        if self.items.len() + local.pending.len() >= 8 {
            return Err(ProviderError::SourceLedgerLimit);
        }
        local.ordinal = local
            .ordinal
            .checked_add(1)
            .filter(|n| *n <= 4096)
            .ok_or(ProviderError::SourceLedgerLimit)?;
        local.last_end = boundary.end_frame;
        local.active = None;
        let commit = LocalCommit {
            boundary,
            ordinal: local.ordinal,
            sent: Instant::now(),
        };
        let reason = match commit.boundary.reason {
            CommitReason::Silence => "silence",
            CommitReason::FinishInput => "finish",
        };
        let event = serde_json::json!({"type":"input_audio_buffer.commit","event_id":format!("pilot_commit_{}_{}",commit.ordinal,reason)});
        local.pending.push_back(commit);
        Ok(event)
    }
    fn local_receipt(&mut self, v: &Value) -> Result<Vec<StreamEvent>, ProviderError> {
        let id = field(v, "item_id")?;
        if !super::protocol::safe_id(id) || self.seen.contains(id) {
            return Err(ProviderError::Protocol);
        }
        if let Some(previous) = v.get("previous_item_id")
            && !(previous.is_null() && self.last_committed.is_none()
                || previous
                    .as_str()
                    .is_some_and(|id| Some(id) == self.last_committed.as_deref()))
        {
            return Err(ProviderError::Protocol);
        }
        let local = self.local.as_mut().ok_or(ProviderError::Protocol)?;
        let commit = local.pending.pop_front().ok_or(ProviderError::Protocol)?;
        if commit.sent.elapsed() >= MAX_PENDING_AGE {
            return Err(ProviderError::SourceResponseDeadline);
        }
        if commit.boundary.end_frame <= commit.boundary.start_frame {
            return Err(ProviderError::Protocol);
        }
        let early = local.early.remove(id);
        self.register(id, commit.boundary.start_frame as f64 * 20.0)?;
        let item = self.items.back_mut().ok_or(ProviderError::Protocol)?;
        if item.sequence != commit.ordinal {
            return Err(ProviderError::Protocol);
        }
        item.committed = true;
        item.stopped = true;
        item.pending_since = Some(commit.sent);
        item.committed_frames = Some(commit.boundary.end_frame - commit.boundary.start_frame);
        item.commit_reason = Some(commit.boundary.reason);
        self.last_committed = Some(id.into());
        if let Some(draft) = early {
            item.partial = draft.text.clone();
            // Retirement removes only the transient, unbound presentation. The
            // following canonical item receives its ordinal from this receipt,
            // never from a draft's arrival order. Neither event re-charges text.
            return Ok(vec![
                StreamEvent::SourceDraft {
                    item_id: id.into(),
                    revision: draft.revision,
                    text: String::new(),
                    retired: true,
                },
                Self::event(item, draft.text, false)?,
            ]);
        }
        Ok(vec![])
    }
    fn early_delta(&mut self, v: &Value) -> Result<Vec<StreamEvent>, ProviderError> {
        let delta = bounded_text(v, "delta")?;
        let id = field(v, "item_id")?;
        let local = self.local.as_mut().ok_or(ProviderError::Protocol)?;
        if !super::protocol::safe_id(id)
            || self.seen.contains(id)
            || (local.active.is_none() && local.pending.is_empty())
        {
            return Err(ProviderError::Protocol);
        }
        // Empty deltas have no visible information. They must not allocate an
        // orphan item, consume revision capacity or turn an empty drain pending.
        if delta.is_empty() {
            return Ok(vec![]);
        }
        if !local.early.contains_key(id) && local.early.len() >= 8 {
            return Err(ProviderError::SourceLedgerLimit);
        }
        let draft = local.early.entry(id.into()).or_insert_with(|| EarlyDraft {
            text: String::new(),
            revision: 0,
            expires: Instant::now() + MAX_TURN + MAX_PENDING_AGE,
        });
        self.text_charge = delta.chars().count();
        if draft.text.len() + delta.len() > 16384
            || draft.text.chars().count() + self.text_charge > 4096
            || draft.revision >= 4096
        {
            return Err(ProviderError::TranscriptItemLimit);
        }
        draft.revision += 1;
        draft.text.push_str(&delta);
        // This is deliberately a separate provisional channel: no source
        // ordinal, finality, archive or analysis authority exists before receipt.
        Ok(vec![StreamEvent::SourceDraft {
            item_id: id.into(),
            revision: draft.revision,
            text: draft.text.clone(),
            retired: false,
        }])
    }
    pub fn complete(&self) -> bool {
        self.items.is_empty()
            && self.manual_commits == 0
            && self.active.is_none()
            && self
                .local
                .as_ref()
                .is_none_or(|l| l.active.is_none() && l.pending.is_empty() && l.early.is_empty())
    }
    fn timed_limit(&self) -> Option<(Instant, ProviderError)> {
        self.items
            .iter()
            .map(|item| {
                if self.active.as_deref() == Some(&item.id) {
                    (item.entered + MAX_TURN, ProviderError::SpeechBoundaryLimit)
                } else {
                    // Waiting for a final begins at the first speech-stop or
                    // commit receipt, not at the beginning of a long utterance.
                    (
                        item.pending_since.unwrap_or(item.entered) + MAX_PENDING_AGE,
                        ProviderError::SourceResponseDeadline,
                    )
                }
            })
            .chain(self.local.iter().flat_map(|l| {
                l.active
                    .iter()
                    .map(|a| (a.entered + MAX_TURN, ProviderError::SpeechBoundaryLimit))
                    .chain(l.pending.iter().map(|c| {
                        (
                            c.sent + MAX_PENDING_AGE,
                            ProviderError::SourceResponseDeadline,
                        )
                    }))
                    .chain(
                        l.early
                            .values()
                            .map(|draft| (draft.expires, ProviderError::SourceResponseDeadline)),
                    )
            }))
            .min_by_key(|(at, _)| *at)
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.timed_limit().map(|(at, _)| at)
    }
    pub fn check(&self, frames: u64) -> Result<(), ProviderError> {
        if let Some((at, error)) = self.timed_limit()
            && Instant::now() >= at
        {
            return Err(error);
        }
        if self.items.iter().any(|item| {
            self.active.as_deref() == Some(&item.id)
                && frames.saturating_mul(20) as f64 - item.start_ms >= MAX_TURN.as_millis() as f64
        }) {
            return Err(ProviderError::SpeechBoundaryLimit);
        }
        Ok(())
    }
    #[cfg(test)]
    pub fn manual_commit(&mut self) -> Result<Value, ProviderError> {
        if self.items.len() + self.manual_commits as usize >= 8 {
            return Err(ProviderError::SourceLedgerLimit);
        }
        self.manual_commits += 1;
        Ok(serde_json::json!({"type":"input_audio_buffer.commit"}))
    }
    fn register(&mut self, id: &str, start_ms: f64) -> Result<(), ProviderError> {
        if !super::protocol::safe_id(id) || self.seen.contains(id) {
            return Err(ProviderError::Protocol);
        }
        if self.items.len() >= 8 || self.seen.len() >= 4096 {
            return Err(ProviderError::SourceLedgerLimit);
        }
        self.seen.insert(id.to_owned());
        self.items.push_back(Item {
            id: id.into(),
            sequence: self.seen.len() as u32,
            revision: 0,
            partial: String::new(),
            final_text: None,
            committed: false,
            stopped: false,
            start_ms,
            entered: Instant::now(),
            pending_since: None,
            committed_frames: None,
            commit_reason: None,
        });
        Ok(())
    }
    fn event(
        item: &mut Item,
        text: String,
        final_fragment: bool,
    ) -> Result<StreamEvent, ProviderError> {
        item.revision = item
            .revision
            .checked_add(1)
            .filter(|r| *r <= 4096)
            .ok_or(ProviderError::TranscriptItemLimit)?;
        Ok(StreamEvent::Transcript {
            provenance: Provenance::SourceAsr,
            text,
            final_fragment,
            item: Some(TranscriptItem {
                id: item.id.clone(),
                sequence: item.sequence,
                revision: item.revision,
                source_audio: if final_fragment {
                    item.committed_frames
                        .map(|frames| {
                            let admitted_audio_ms = frames
                                .checked_mul(20)
                                .and_then(|ms| u32::try_from(ms).ok())
                                .ok_or(ProviderError::Protocol)?;
                            Ok(SourceAudioEvidence {
                                version: 1,
                                admitted_audio_ms,
                                boundary: match item.commit_reason.ok_or(ProviderError::Protocol)? {
                                    CommitReason::Silence => SourceAudioBoundary::DetectorSilence,
                                    CommitReason::FinishInput => {
                                        SourceAudioBoundary::ExplicitFinish
                                    }
                                },
                                detector: SourceAudioDetector::ManualLocalBoundariesV1,
                            })
                        })
                        .transpose()?
                } else {
                    None
                },
            }),
        })
    }
    pub fn consume(&mut self, v: Value, frames: u64) -> Result<Vec<StreamEvent>, ProviderError> {
        self.text_charge = 0;
        self.check(frames)?;
        let kind = field(&v, "type")?;
        if self.local.is_some() {
            if matches!(
                kind,
                "input_audio_buffer.speech_started" | "input_audio_buffer.speech_stopped"
            ) {
                return Err(ProviderError::Protocol);
            }
            if kind == "input_audio_buffer.committed" {
                return self.local_receipt(&v);
            }
            if kind == "conversation.item.input_audio_transcription.delta"
                && !self
                    .items
                    .iter()
                    .any(|i| Some(i.id.as_str()) == v["item_id"].as_str())
            {
                if v["content_index"] != 0 {
                    return Err(ProviderError::Protocol);
                }
                return self.early_delta(&v);
            }
        }
        match kind {
            "input_audio_buffer.speech_started" => {
                let id = field(&v, "item_id")?;
                let start = timestamp(&v, "audio_start_ms", frames)?;
                if self.active.is_some() || start < self.last_start {
                    return Err(ProviderError::Protocol);
                }
                self.register(id, start)?;
                self.last_start = start;
                self.active = Some(id.into());
                Ok(vec![])
            }
            "input_audio_buffer.speech_stopped" => {
                let id = field(&v, "item_id")?;
                let end = timestamp(&v, "audio_end_ms", frames)?;
                if self.active.as_deref() != Some(id) {
                    return Err(ProviderError::Protocol);
                }
                let item = self
                    .items
                    .iter_mut()
                    .find(|i| i.id == id)
                    .ok_or(ProviderError::Protocol)?;
                if end < item.start_ms || item.stopped || item.committed {
                    return Err(ProviderError::Protocol);
                }
                item.stopped = true;
                item.pending_since.get_or_insert_with(Instant::now);
                self.active = None;
                Ok(vec![])
            }
            "input_audio_buffer.committed" => {
                let id = field(&v, "item_id")?;
                if let Some(previous) = v.get("previous_item_id") {
                    let valid = if previous.is_null() {
                        self.last_committed.is_none()
                    } else {
                        previous.as_str().is_some_and(|id| {
                            super::protocol::safe_id(id)
                                && Some(id) == self.last_committed.as_deref()
                        })
                    };
                    if !valid {
                        return Err(ProviderError::Protocol);
                    }
                }
                if !self.items.iter().any(|i| i.id == id) {
                    if self.manual_commits == 0 {
                        return Err(ProviderError::Protocol);
                    }
                    self.register(id, 0.0)?;
                }
                let item = self
                    .items
                    .iter_mut()
                    .find(|i| i.id == id)
                    .ok_or(ProviderError::Protocol)?;
                if item.committed || (!item.stopped && self.manual_commits == 0) {
                    return Err(ProviderError::Protocol);
                }
                item.committed = true;
                item.pending_since.get_or_insert_with(Instant::now);
                if self.manual_commits > 0 {
                    self.manual_commits -= 1;
                }
                self.last_committed = Some(id.into());
                Ok(vec![])
            }
            "conversation.item.input_audio_transcription.delta" => {
                if v["content_index"] != 0 {
                    return Err(ProviderError::Protocol);
                }
                let delta = bounded_text(&v, "delta")?;
                let id = field(&v, "item_id")?;
                let item = self
                    .items
                    .iter_mut()
                    .find(|i| i.id == id)
                    .ok_or(ProviderError::Protocol)?;
                if item.final_text.is_some() {
                    return Err(ProviderError::Protocol);
                }
                if delta.is_empty() {
                    return Ok(vec![]);
                }
                self.text_charge = delta.chars().count();
                if item.partial.len() + delta.len() > 16384
                    || item.partial.chars().count() + self.text_charge > 4096
                {
                    return Err(ProviderError::TranscriptItemLimit);
                }
                item.partial.push_str(&delta);
                let text = item.partial.clone();
                Ok(vec![Self::event(item, text, false)?])
            }
            "conversation.item.input_audio_transcription.completed" => {
                if v["content_index"] != 0 {
                    return Err(ProviderError::Protocol);
                }
                let text = bounded_text(&v, "transcript")?;
                let id = field(&v, "item_id")?;
                let item = self
                    .items
                    .iter_mut()
                    .find(|i| i.id == id)
                    .ok_or(ProviderError::Protocol)?;
                if !item.committed || item.final_text.is_some() || v["usage"]["type"] != "duration"
                {
                    return Err(ProviderError::Protocol);
                }
                let seconds = v["usage"]["seconds"]
                    .as_f64()
                    .filter(|n| n.is_finite() && *n >= 0.0)
                    .ok_or(ProviderError::Protocol)?;
                let next_usage = self.usage + seconds;
                if self.local.is_some() {
                    let committed_frames = item.committed_frames.ok_or(ProviderError::Protocol)?;
                    let submitted_ms = committed_frames
                        .checked_mul(20)
                        .and_then(|milliseconds| u32::try_from(milliseconds).ok())
                        .ok_or(ProviderError::Protocol)?;
                    let allowed_ms = submitted_ms
                        .checked_add(ITEM_USAGE_TOLERANCE_MS)
                        .ok_or(ProviderError::Protocol)?;
                    if seconds > f64::from(allowed_ms) / 1000.0 {
                        return Err(ProviderError::AsrUsageMismatch {
                            submitted_ms,
                            reported_ms: reported_millis(seconds),
                            allowed_ms,
                        });
                    }
                } else if next_usage
                    > frames as f64 / 50.0 + f64::from(ITEM_USAGE_TOLERANCE_MS) / 1000.0
                {
                    // Retained server-VAD traces do not have a local commit
                    // frame witness; preserve their existing aggregate guard.
                    return Err(ProviderError::UsageMismatch);
                }
                if !next_usage.is_finite() {
                    return Err(ProviderError::UsageMismatch);
                }
                // Reported per-item usage is retained independently of selected
                // PCM duration. Neither value is a measured billing total.
                self.usage = next_usage;
                self.text_charge = text.chars().count();
                // Classification belongs to derived outcomes, never the raw text.
                item.final_text = Some(text);
                let mut out = vec![];
                while self.items.front().is_some_and(|i| i.final_text.is_some()) {
                    let mut item = self.items.pop_front().ok_or(ProviderError::Protocol)?;
                    let text = item.final_text.take().ok_or(ProviderError::Protocol)?;
                    out.push(Self::event(&mut item, text, true)?);
                }
                Ok(out)
            }
            "conversation.item.added" | "conversation.item.created" | "conversation.item.done" => {
                if v["item"]["role"] != "user"
                    || v["item"]["type"] != "message"
                    || !super::protocol::safe_id(field(&v["item"], "id")?)
                {
                    return Err(ProviderError::Protocol);
                }
                Ok(vec![])
            }
            "rate_limits.updated" => Ok(vec![]),
            _ => Err(ProviderError::Protocol),
        }
    }
}

#[cfg(test)]
mod presentation_tests;

#[cfg(test)]
mod capacity_tests;

#[cfg(test)]
mod usage_tests;

#[cfg(test)]
mod draft_tests;
