//! Backend-neutral, bounded provider boundary. Implementations own upstream close.
//! Runtime injection is explicit; there is no browser endpoint/config discovery.
use serde::{Deserialize, Serialize};
use std::{future::Future, pin::Pin, sync::Arc};
use tokio::sync::{mpsc, watch};

pub use crate::interpretation::Interpretation;

pub type ProviderFuture<T> = Pin<Box<dyn Future<Output = Result<T, ProviderError>> + Send>>;

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Language {
    Ja,
    En,
    Hi,
    Es,
    Vi,
}
impl Language {
    pub fn legacy(self) -> bool {
        matches!(self, Self::Ja | Self::En | Self::Hi)
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Ja => "Japanese",
            Self::En => "English",
            Self::Hi => "Hindi",
            Self::Es => "Spanish",
            Self::Vi => "Vietnamese",
        }
    }
}

/// Explicit session-only ASR hints. Literal terms are data, never instructions.
#[derive(Clone, Deserialize, Serialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SpeechHints {
    pub languages: Vec<Language>,
    pub keywords: Vec<String>,
}
impl Default for SpeechHints {
    fn default() -> Self {
        Self {
            languages: vec![Language::Ja, Language::En, Language::Hi],
            keywords: Vec::new(),
        }
    }
}
impl SpeechHints {
    pub fn validate(&self) -> Result<(), ProviderError> {
        if self.languages.is_empty() || self.languages.len() > 5
            || self.languages.iter().enumerate().any(|(i, language)| self.languages[..i].contains(language))
            || self.keywords.len() > 20 || self.keywords.iter().map(String::len).sum::<usize>() > 1024
            || self.keywords.iter().enumerate().any(|(i, word)| word.is_empty() || word.trim() != word
                || word.chars().count() > 80 || self.keywords[..i].contains(word)
                || word.chars().any(|c| c.is_control() || matches!(c, '<' | '>' | '\u{200b}' | '\u{200e}' | '\u{200f}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}' | '\u{feff}')))
        { return Err(ProviderError::Protocol); }
        Ok(())
    }
}

#[derive(Clone, Default)]
pub struct RouteOptions {
    pub live_mode: Option<LiveMode>,
    pub transcription: Option<SpeechHints>,
    pub interpretation: Option<Interpretation>,
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Route {
    Transcription,
    Translation,
    Live,
}

/// Immutable behavior within the Live provider route, not a separate billing
/// route or permission to expose generated speech to another audience.
#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum LiveMode {
    Assistant,
    Interpretation,
}

impl Route {
    /// Older callers mean assistant. Interpretation always requires an explicit
    /// value, and modes are forbidden on non-Live services.
    pub fn resolve_live_mode(
        self,
        mode: Option<LiveMode>,
    ) -> Result<Option<LiveMode>, ProviderError> {
        match (self, mode) {
            (Self::Live, mode) => Ok(Some(mode.unwrap_or(LiveMode::Assistant))),
            (_, None) => Ok(None),
            _ => Err(ProviderError::Protocol),
        }
    }
}

#[derive(Clone, Copy, Debug, Deserialize, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Provenance {
    SourceAsr,
    LiveInput,
    LiveOutput,
    Translation,
}

pub enum StreamInput {
    Pcm16(Vec<u8>),
    Text(String),
    /// End input without invoking immediate Stop. No later input is accepted.
    /// The bounded evaluator may observe results until Drained then Closed.
    FinishInput,
    /// Internal continuous ASR handover: wait for a natural local boundary.
    /// Unlike explicit FinishInput, never commits an active utterance on a timer.
    FinishAtBoundary,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct TranscriptItem {
    pub id: String,
    pub revision: u32,
    pub sequence: u32,
    /// Exact locally committed source duration, bound after the provider receipt.
    /// Internal evidence only: the canonical operator transcript wire is unchanged.
    #[serde(skip)]
    pub source_audio: Option<SourceAudioEvidence>,
}

#[derive(Clone, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct SourceAudioEvidence {
    pub version: u8,
    pub admitted_audio_ms: u32,
    pub boundary: SourceAudioBoundary,
    pub detector: SourceAudioDetector,
}
#[cfg(test)]
impl SourceAudioEvidence {
    pub(crate) fn valid(&self) -> bool {
        self.version == 1
            && (100..120_000).contains(&self.admitted_audio_ms)
            && self.admitted_audio_ms.is_multiple_of(20)
    }
}
#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", try_from = "String")]
pub enum SourceAudioBoundary {
    /// Endpoint trigger after selected audio; never proof that no speech occurred.
    DetectorSilence,
    ExplicitFinish,
}
impl TryFrom<String> for SourceAudioBoundary {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "detector_silence" => Ok(Self::DetectorSilence),
            "explicit_finish" => Ok(Self::ExplicitFinish),
            _ => Err("invalid source audio boundary"),
        }
    }
}
#[derive(Clone, Copy, Debug, Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case", try_from = "String")]
pub enum SourceAudioDetector {
    ManualLocalBoundariesV1,
}
impl TryFrom<String> for SourceAudioDetector {
    type Error = &'static str;
    fn try_from(value: String) -> Result<Self, Self::Error> {
        match value.as_str() {
            "manual_local_boundaries_v1" => Ok(Self::ManualLocalBoundariesV1),
            _ => Err("invalid source audio detector"),
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DrainCompletion {
    /// All detected/committed ASR items finalized. Not proof of speech coverage.
    CommittedTranscriptsFinal,
    /// Translation flush completed with its exact terminal event; not language accuracy.
    ProviderSessionClosed,
    /// No semantic completion marker; output may have been cut off.
    ObservationWindowEnded,
}

pub enum StreamEvent {
    Ready {
        expires_in_seconds: u64,
    },
    /// Unbound provider hypothesis. Never source chronology, evidence or export.
    SourceDraft {
        item_id: String,
        revision: u32,
        text: String,
        retired: bool,
    },
    /// Provider timeline interval; not a source item, turn or playback receipt.
    LiveTranscript {
        provenance: Provenance,
        text: String,
        start_ms: f64,
        end_ms: f64,
    },
    Transcript {
        provenance: Provenance,
        text: String,
        final_fragment: bool,
        /// Required for source ASR; null for generated append-only deltas.
        item: Option<TranscriptItem>,
    },
    Audio {
        pcm16: Vec<u8>,
        sample_rate: u32,
    },
    /// A planned continuous-provider handover; legacy providers never emit it.
    Renewing {
        window: u64,
    },
    /// The next window replaces an interrupted child; no source completion is implied.
    Recovering {
        window: u64,
        error: ProviderError,
        retry_in_seconds: u64,
    },
    /// Planned handover of the current finite provider window.
    Handover,
    /// Private transport receipt, consumed by the continuous supervisor only.
    InputSealed,
    /// Provider budget uncertainty must be an error, not invented zero usage.
    Error(ProviderError),
    Drained {
        completion: DrainCompletion,
    },
    Closed {
        confirmed: bool,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderEndReason {
    CloseRequested,
    Expired,
    Content,
    RemoteHangup,
    ConnectionLost,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProviderError {
    Disabled,
    Unavailable,
    ConnectionLost,
    InputInterrupted,
    Protocol,
    Budget,
    TextLimit,
    StreamLimit,
    InputAudioLimit,
    OutputAudioLimit,
    InputPacing,
    SpeechBoundaryLimit,
    SourceLedgerLimit,
    TranscriptItemLimit,
    UsageMismatch,
    AsrUsageMismatch {
        submitted_ms: u32,
        reported_ms: Option<u64>,
        allowed_ms: u32,
    },
    SourceResponseDeadline,
    SourceIncomplete,
    ProtocolCapacityLimit,
    Deadline,
    SessionEnded(ProviderEndReason),
}

impl ProviderError {
    /// Only operational interruptions may reuse a still-active continuous Start.
    /// Auth, malformed protocol, accounting, consent and resource faults cannot.
    pub fn recoverable(self) -> bool {
        matches!(
            self,
            Self::ConnectionLost
                | Self::InputInterrupted
                | Self::SessionEnded(
                    ProviderEndReason::Expired
                        | ProviderEndReason::RemoteHangup
                        | ProviderEndReason::ConnectionLost
                )
        )
    }
    pub fn code(self) -> &'static str {
        match self {
            Self::Disabled | Self::Unavailable => "provider_unavailable",
            Self::ConnectionLost => "provider_connection_lost",
            Self::InputInterrupted => "source_input_interrupted",
            Self::Protocol => "provider_protocol",
            Self::Budget => "budget_exhausted",
            Self::TextLimit => "text_limit",
            Self::StreamLimit => "stream_limit",
            Self::InputAudioLimit => "input_audio_limit",
            Self::OutputAudioLimit => "output_audio_limit",
            Self::InputPacing => "input_pacing",
            Self::SpeechBoundaryLimit => "speech_boundary_limit",
            Self::SourceLedgerLimit => "source_ledger_limit",
            Self::TranscriptItemLimit => "transcript_item_limit",
            Self::UsageMismatch | Self::AsrUsageMismatch { .. } => "usage_mismatch",
            Self::SourceResponseDeadline => "source_response_deadline",
            Self::SourceIncomplete => "source_incomplete",
            Self::ProtocolCapacityLimit => "protocol_capacity_limit",
            Self::Deadline => "deadline",
            Self::SessionEnded(ProviderEndReason::CloseRequested) => "live_closed_requested",
            Self::SessionEnded(ProviderEndReason::Expired) => "live_expired",
            Self::SessionEnded(ProviderEndReason::Content) => "live_content_closed",
            Self::SessionEnded(ProviderEndReason::RemoteHangup) => "live_remote_hangup",
            Self::SessionEnded(ProviderEndReason::ConnectionLost) => "live_connection_lost",
        }
    }
}

/// Bounded causal metadata for a paired backend. Never contains upstream text,
/// identifiers, credentials or authority to restart provider work.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ProviderFailure {
    pub route: Route,
    pub error: ProviderError,
}

/// Drop closes the stop signal independently of a full data channel. Provider
/// tasks must retain finite connect/read/close deadlines even if their consumer
/// disappears. Queue capacities are at most eight inputs and thirty-two events.
pub struct StreamHandle {
    pub input: mpsc::Sender<StreamInput>,
    pub events: mpsc::Receiver<StreamEvent>,
    pub stop: watch::Sender<bool>,
    /// Stops future window admission immediately; the ordered FinishInput
    /// barrier still decides when current source audio ends.
    pub renewal_fence: Option<watch::Sender<bool>>,
}

impl Drop for StreamHandle {
    fn drop(&mut self) {
        self.stop.send_replace(true);
    }
}

pub struct PreparedAudio {
    pub wav: Vec<u8>,
    pub duration_ms: u64,
    pub sample_rate: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderActivity {
    NoneAdmitted,
    CloseObserved,
    #[default]
    Unconfirmed,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct ProviderStopEvidence {
    pub activity: ProviderActivity,
    pub reserved_microusd: Option<u64>,
}

pub trait ProviderBackend: Send + Sync {
    fn vendor(&self) -> crate::budget::ProviderVendor {
        crate::budget::ProviderVendor::OpenAi
    }
    /// Synchronous negative authority before any asynchronous retirement.
    fn revoke(&self) {}

    /// Meeting-scoped observation after the admission fence, including every
    /// paid route and speech synthesis request. A backend with other owners
    /// (continuous windows and direct WebRTC) defaults to unknown.
    fn stop_evidence(&self) -> ProviderStopEvidence {
        ProviderStopEvidence::default()
    }

    /// True only for explicitly approved configuration. This is availability,
    /// never a statement that live language/account quality has been verified.
    fn enabled(&self) -> bool;
    fn supports(&self, _route: Route) -> bool {
        self.enabled()
    }
    fn supports_tts(&self) -> bool {
        self.enabled()
    }
    /// Existing adapters do not acquire interpreter behavior implicitly.
    fn supports_live_mode(&self, mode: LiveMode) -> bool {
        mode == LiveMode::Assistant && self.supports(Route::Live)
    }
    /// Synchronous local metadata only. No provider/key/ledger-file access.
    /// An implementation without accounting returns unknown, never zero spent.
    fn budget_snapshot(&self) -> crate::budget::BudgetSnapshot {
        crate::budget::BudgetSnapshot::inactive(crate::budget::BudgetState::Unavailable)
    }
    /// First local failure, retained independently of event delivery/cleanup.
    /// Adapters without causal metadata return unknown rather than guessing.
    fn failure_cause(&self) -> Option<ProviderFailure> {
        None
    }
    fn open(&self, route: Route, language: Language) -> ProviderFuture<StreamHandle>;
    fn open_with_mode(
        &self,
        route: Route,
        language: Language,
        mode: Option<LiveMode>,
    ) -> ProviderFuture<StreamHandle> {
        match route.resolve_live_mode(mode) {
            Ok(None | Some(LiveMode::Assistant)) => self.open(route, language),
            _ => Box::pin(async { Err(ProviderError::Unavailable) }),
        }
    }
    /// Versioned interpretation is opt-in per adapter and immutable per stream.
    fn open_interpretation(
        &self,
        _language: Language,
        _policy: Interpretation,
    ) -> ProviderFuture<StreamHandle> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
    /// Optional literal context and voice are immutable for this interpretation.
    /// Older adapters reject configured context instead of silently ignoring it.
    fn open_interpretation_context(
        &self,
        language: Language,
        policy: Interpretation,
        context: crate::interpretation_context::InterpretationContext,
    ) -> ProviderFuture<StreamHandle> {
        if context != crate::interpretation_context::InterpretationContext::default() {
            return Box::pin(async { Err(ProviderError::Unavailable) });
        }
        self.open_interpretation(language, policy)
    }
    /// Legacy/custom backends must explicitly support non-default hints.
    fn open_transcription(
        &self,
        language: Language,
        hints: SpeechHints,
    ) -> ProviderFuture<StreamHandle> {
        if hints != SpeechHints::default() {
            return Box::pin(async { Err(ProviderError::Unavailable) });
        }
        self.open(Route::Transcription, language)
    }
    fn synthesize(&self, text: String, language: Language) -> ProviderFuture<PreparedAudio>;
}

pub struct DisabledBackend;
impl ProviderBackend for DisabledBackend {
    fn stop_evidence(&self) -> ProviderStopEvidence {
        ProviderStopEvidence {
            activity: ProviderActivity::NoneAdmitted,
            reserved_microusd: Some(0),
        }
    }
    fn enabled(&self) -> bool {
        false
    }
    fn budget_snapshot(&self) -> crate::budget::BudgetSnapshot {
        crate::budget::BudgetSnapshot::inactive(crate::budget::BudgetState::Disabled)
    }
    fn open(&self, _: Route, _: Language) -> ProviderFuture<StreamHandle> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Disabled) })
    }
}

pub fn disabled() -> Arc<dyn ProviderBackend> {
    Arc::new(DisabledBackend)
}
