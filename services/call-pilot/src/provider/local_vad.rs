//! Deterministic local utterance boundaries over original 16kHz PCM16 frames.
//!
//! The activity input MUST come from the explicitly configured, vetted local
//! speech detector. This module does not equate sample energy with speech and
//! never claims semantic completeness or language accuracy. No network or clock
//! is used: source frame indices account for every admitted 20ms input frame.
use std::collections::VecDeque;

pub(super) const FRAME_BYTES: usize = 640;
pub(super) const PRE_ROLL_FRAMES: usize = 15;
pub(super) const SILENCE_FRAMES: usize = 25;
pub(super) const MAX_TURN_FRAMES: u64 = 6000;
/// Conservative local admission policy, not a claimed provider minimum.
pub(super) const MIN_COMMIT_FRAMES: u64 = 5;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BoundaryError {
    InvalidFrame,
    Closed,
    FrameOverflow,
    TurnLimit,
    IncompleteCandidate,
    DetectorUnavailable,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum CommitReason {
    Silence,
    FinishInput,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct CommitBoundary {
    /// Half-open ORIGINAL source-frame range, not provider audio timestamps.
    pub start_frame: u64,
    pub end_frame: u64,
    pub reason: CommitReason,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Segment {
    /// Selected original PCM16. At most15frames; each source sample is emitted
    /// at most once. Resampling remains a separate, stateful transport step.
    pub append: Vec<u8>,
    /// Range of append; start==end when there are no new bytes (e.g. FinishInput).
    pub start_frame: u64,
    pub end_frame: u64,
    /// Send only AFTER all append bytes were accepted by the upstream transport.
    /// This is a local boundary proposal, never a provider-final receipt.
    pub commit: Option<CommitBoundary>,
}

struct Frame {
    index: u64,
    bytes: [u8; FRAME_BYTES],
    speech: bool,
}

#[derive(Default)]
pub(super) struct Segmenter {
    pre_roll: VecDeque<Frame>,
    onset: u8,
    trailing_silence: usize,
    active_start: Option<u64>,
    next_frame: u64,
    closed: bool,
}

/// Pinned libfvad/WebRTC detector in Quality mode. One mutable detector belongs
/// to one route; it is never shared concurrently or reset through raw pointers.
pub(crate) struct LocalVad {
    detector: fvad::Fvad,
    segmenter: Segmenter,
}

impl LocalVad {
    pub fn new() -> Result<Self, BoundaryError> {
        let detector = fvad::Fvad::new()
            .ok_or(BoundaryError::DetectorUnavailable)?
            .set_sample_rate(fvad::SampleRate::Rate16kHz)
            .set_mode(fvad::Mode::Quality);
        Ok(Self {
            detector,
            segmenter: Segmenter::new(),
        })
    }

    pub(super) fn push(&mut self, pcm16: &[u8]) -> Result<Segment, BoundaryError> {
        if self.segmenter.closed {
            return Err(BoundaryError::Closed);
        }
        if pcm16.len() != FRAME_BYTES {
            return self.segmenter.reject(BoundaryError::InvalidFrame);
        }
        let speech = match self.classify_frame(pcm16) {
            Ok(speech) => speech,
            Err(error) => return self.segmenter.reject(error),
        };
        self.segmenter.push(pcm16, speech)
    }

    fn classify_frame(&mut self, pcm16: &[u8]) -> Result<bool, BoundaryError> {
        if pcm16.len() != FRAME_BYTES {
            return Err(BoundaryError::InvalidFrame);
        }
        let mut samples = [0i16; 320];
        for (sample, bytes) in samples.iter_mut().zip(pcm16.chunks_exact(2)) {
            *sample = i16::from_le_bytes([bytes[0], bytes[1]]);
        }
        self.detector
            .is_voice_frame(&samples)
            .ok_or(BoundaryError::DetectorUnavailable)
    }

    pub(super) fn finish(&mut self) -> Result<Segment, BoundaryError> {
        self.segmenter.finish()
    }

    pub fn stop(&mut self) {
        self.segmenter.stop();
    }

    pub fn active(&self) -> bool {
        self.segmenter.active()
    }
}

impl Segmenter {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn active(&self) -> bool {
        !self.closed && self.active_start.is_some()
    }

    fn empty(&self) -> Segment {
        Segment {
            append: Vec::new(),
            start_frame: self.next_frame,
            end_frame: self.next_frame,
            commit: None,
        }
    }

    fn reject<T>(&mut self, error: BoundaryError) -> Result<T, BoundaryError> {
        self.stop();
        Err(error)
    }

    /// Two detector-positive frames among the latest three establish onset.
    /// Once active, append through25consecutive detector-negative tail frames.
    /// These are explicit configuration choices requiring offline evaluation;
    /// they are not claims that all short words/noisy speech are recognized.
    pub fn push(&mut self, pcm16: &[u8], is_speech: bool) -> Result<Segment, BoundaryError> {
        if self.closed {
            return Err(BoundaryError::Closed);
        }
        let Ok(bytes) = <[u8; FRAME_BYTES]>::try_from(pcm16) else {
            return self.reject(BoundaryError::InvalidFrame);
        };
        let frame = self.next_frame;
        let Some(next) = frame.checked_add(1) else {
            return self.reject(BoundaryError::FrameOverflow);
        };
        self.next_frame = next;
        if let Some(start) = self.active_start {
            // Never manufacture a final or split continuous speech at120s.
            // The caller closes with an explicit incomplete/gap condition.
            if next - start >= MAX_TURN_FRAMES {
                return self.reject(BoundaryError::TurnLimit);
            }
            self.trailing_silence = if is_speech {
                0
            } else {
                self.trailing_silence + 1
            };
            let commit = if self.trailing_silence >= SILENCE_FRAMES {
                self.active_start = None;
                self.trailing_silence = 0;
                self.onset = 0;
                Some(CommitBoundary {
                    start_frame: start,
                    end_frame: next,
                    reason: CommitReason::Silence,
                })
            } else {
                None
            };
            return Ok(Segment {
                append: bytes.to_vec(),
                start_frame: frame,
                end_frame: next,
                commit,
            });
        }

        self.pre_roll.push_back(Frame {
            index: frame,
            bytes,
            speech: is_speech,
        });
        if self.pre_roll.len() > PRE_ROLL_FRAMES {
            self.pre_roll.pop_front();
        }
        self.onset = ((self.onset << 1) | u8::from(is_speech)) & 0b111;
        if self.onset.count_ones() < 2 {
            return Ok(self.empty());
        }
        let start = self
            .pre_roll
            .front()
            .expect("current frame was inserted")
            .index;
        let mut append = Vec::with_capacity(self.pre_roll.len() * FRAME_BYTES);
        for frame in self.pre_roll.drain(..) {
            append.extend_from_slice(&frame.bytes);
        }
        self.active_start = Some(start);
        self.trailing_silence = 0;
        Ok(Segment {
            append,
            start_frame: start,
            end_frame: next,
            commit: None,
        })
    }

    /// Close input exactly once. Never pads fabricated silence or sends empty
    /// provider commits. A recent sub-onset speech candidate is explicitly
    /// incomplete rather than silently declared fully drained.
    pub fn finish(&mut self) -> Result<Segment, BoundaryError> {
        if self.closed {
            return Err(BoundaryError::Closed);
        }
        let candidate = self.active_start.is_none() && self.pre_roll.iter().any(|f| f.speech);
        if candidate
            || self
                .active_start
                .is_some_and(|start| self.next_frame - start < MIN_COMMIT_FRAMES)
        {
            return self.reject(BoundaryError::IncompleteCandidate);
        }
        let mut output = self.empty();
        output.commit = self.active_start.map(|start| CommitBoundary {
            start_frame: start,
            end_frame: self.next_frame,
            reason: CommitReason::FinishInput,
        });
        self.stop();
        Ok(output)
    }

    /// Sticky negative action: discard unsent pre-roll, never append or commit.
    pub fn stop(&mut self) {
        self.closed = true;
        self.pre_roll.clear();
        self.onset = 0;
        self.active_start = None;
        self.trailing_silence = 0;
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pcm(index: u16) -> [u8; FRAME_BYTES] {
        let mut bytes = [0; FRAME_BYTES];
        for sample in bytes.chunks_exact_mut(2) {
            sample.copy_from_slice(&index.to_le_bytes());
        }
        bytes
    }

    #[test]
    fn long_idle_is_bounded_and_produces_no_commits() {
        let mut s = Segmenter::new();
        for i in 0..100_000 {
            let out = s.push(&pcm(i as u16), false).unwrap();
            assert!(out.append.is_empty() && out.commit.is_none());
            assert!(s.pre_roll.len() <= PRE_ROLL_FRAMES);
        }
        assert_eq!(s.pre_roll.len(), PRE_ROLL_FRAMES);
        assert!(s.finish().unwrap().commit.is_none());
    }

    #[test]
    fn onset_includes_exact_preroll_once_and_tail_commits_at25() {
        let mut s = Segmenter::new();
        let mut selected = Vec::new();
        for i in 0..20 {
            assert!(s.push(&pcm(i), false).unwrap().append.is_empty());
        }
        assert!(s.push(&pcm(20), true).unwrap().append.is_empty());
        let first = s.push(&pcm(21), true).unwrap();
        assert_eq!((first.start_frame, first.end_frame), (7, 22));
        assert_eq!(first.append.len(), PRE_ROLL_FRAMES * FRAME_BYTES);
        selected.extend_from_slice(&first.append);
        for i in 22..47 {
            let next = s.push(&pcm(i), false).unwrap();
            assert_eq!(next.commit.is_some(), i == 46);
            if let Some(commit) = next.commit {
                assert_eq!(
                    commit,
                    CommitBoundary {
                        start_frame: 7,
                        end_frame: 47,
                        reason: CommitReason::Silence
                    }
                );
            }
            selected.extend_from_slice(&next.append);
        }
        let expected = (7..47).flat_map(pcm).collect::<Vec<_>>();
        assert_eq!(selected, expected);
        assert!(s.pre_roll.is_empty());
        assert!(s.push(&pcm(47), true).unwrap().append.is_empty());
        let second = s.push(&pcm(48), true).unwrap();
        assert_eq!((second.start_frame, second.end_frame), (47, 49));
        assert_eq!(second.append, [pcm(47), pcm(48)].concat());
    }

    #[test]
    fn separated_clicks_do_not_trigger_but_short_internal_pause_does_not_split() {
        let mut s = Segmenter::new();
        for i in 0..15 {
            assert!(s.push(&pcm(i), i % 4 == 0).unwrap().append.is_empty());
        }
        assert!(s.push(&pcm(15), true).unwrap().append.is_empty());
        assert!(!s.push(&pcm(16), true).unwrap().append.is_empty());
        for i in 17..41 {
            assert!(s.push(&pcm(i), false).unwrap().commit.is_none());
        }
        assert!(s.push(&pcm(41), true).unwrap().commit.is_none());
        for i in 42..66 {
            assert!(s.push(&pcm(i), false).unwrap().commit.is_none());
        }
        assert!(s.push(&pcm(66), false).unwrap().commit.is_some());
    }

    #[test]
    fn finish_active_commits_once_but_never_replays_frames_or_pads_audio() {
        let mut s = Segmenter::new();
        for i in 0..5 {
            s.push(&pcm(i), true).unwrap();
        }
        let end = s.finish().unwrap();
        assert!(end.append.is_empty());
        assert_eq!(
            end.commit,
            Some(CommitBoundary {
                start_frame: 0,
                end_frame: 5,
                reason: CommitReason::FinishInput
            })
        );
        assert_eq!(s.finish().unwrap_err(), BoundaryError::Closed);
        assert_eq!(s.push(&pcm(2), true).unwrap_err(), BoundaryError::Closed);
        let mut candidate = Segmenter::new();
        candidate.push(&pcm(0), true).unwrap();
        assert_eq!(
            candidate.finish().unwrap_err(),
            BoundaryError::IncompleteCandidate
        );
        let mut short = Segmenter::new();
        for i in 0..4 {
            short.push(&pcm(i), true).unwrap();
        }
        assert_eq!(
            short.finish().unwrap_err(),
            BoundaryError::IncompleteCandidate
        );
        assert_eq!(short.finish().unwrap_err(), BoundaryError::Closed);
    }

    #[test]
    fn stop_never_flushes_candidate_or_active_frames() {
        for speech_frames in [0, 1, 2, 10] {
            let mut s = Segmenter::new();
            for i in 0..speech_frames {
                s.push(&pcm(i), true).unwrap();
            }
            s.stop();
            assert!(s.pre_roll.is_empty());
            assert_eq!(s.finish().unwrap_err(), BoundaryError::Closed);
            assert_eq!(s.push(&pcm(0), true).unwrap_err(), BoundaryError::Closed);
        }
    }

    #[test]
    fn malformed_frames_and_counter_overflow_close_locally() {
        let mut s = Segmenter::new();
        assert_eq!(
            s.push(&[0; 639], true).unwrap_err(),
            BoundaryError::InvalidFrame
        );
        assert_eq!(s.push(&pcm(0), true).unwrap_err(), BoundaryError::Closed);
        let mut s = Segmenter {
            next_frame: u64::MAX,
            ..Segmenter::new()
        };
        assert_eq!(
            s.push(&pcm(0), true).unwrap_err(),
            BoundaryError::FrameOverflow
        );
    }

    #[test]
    fn continuous120s_speech_fails_incomplete_instead_of_forced_split() {
        let mut s = Segmenter::new();
        for i in 0..MAX_TURN_FRAMES - 1 {
            assert!(s.push(&pcm(i as u16), true).unwrap().commit.is_none());
        }
        assert_eq!(
            s.push(&pcm(6000), true).unwrap_err(),
            BoundaryError::TurnLimit
        );
        assert_eq!(s.finish().unwrap_err(), BoundaryError::Closed);
    }

    #[test]
    fn actual_pinned_detector_handles_silence_without_empty_commits() {
        let mut vad = LocalVad::new().unwrap();
        for _ in 0..1000 {
            let output = vad.push(&[0; FRAME_BYTES]).unwrap();
            assert!(output.append.is_empty() && output.commit.is_none());
        }
        assert!(!vad.active());
        assert!(vad.finish().unwrap().commit.is_none());
        assert_eq!(
            vad.push(&[0; FRAME_BYTES]).unwrap_err(),
            BoundaryError::Closed
        );
        vad.stop();
    }

    #[test]
    fn synthetic_pulses_have_actual_detector_boundaries_and_exact_selected_bytes() {
        use std::collections::BTreeSet;
        for variant in 0..3 {
            let pcm = crate::test_audio::signal(180, variant);
            let mut frames = pcm
                .chunks(FRAME_BYTES)
                .map(|chunk| {
                    let mut frame = [0; FRAME_BYTES];
                    frame[..chunk.len()].copy_from_slice(chunk);
                    frame
                })
                .collect::<Vec<_>>();
            frames.extend(std::iter::repeat_n([0; FRAME_BYTES], 100));
            let mut vad = LocalVad::new().unwrap();
            let mut selected = BTreeSet::new();
            let mut boundaries = 0;
            for frame in &frames {
                let output = vad.push(frame).unwrap();
                assert_eq!(
                    output.append.len(),
                    (output.end_frame - output.start_frame) as usize * FRAME_BYTES
                );
                assert!(output.append.len() <= PRE_ROLL_FRAMES * FRAME_BYTES);
                for (index, bytes) in (output.start_frame..output.end_frame)
                    .zip(output.append.chunks_exact(FRAME_BYTES))
                {
                    assert!(
                        selected.insert(index),
                        "signal {variant}: a source frame was emitted twice"
                    );
                    assert_eq!(bytes, frames[index as usize]);
                }
                if let Some(commit) = output.commit {
                    assert!(
                        (commit.start_frame..commit.end_frame)
                            .all(|index| selected.contains(&index))
                    );
                    boundaries += 1;
                }
            }
            let end = vad.finish().unwrap();
            assert!(end.append.is_empty() && end.commit.is_none());
            assert!(
                !selected.is_empty() && boundaries > 0,
                "signal {variant}: no actual detector boundary"
            );
        }
    }
}
