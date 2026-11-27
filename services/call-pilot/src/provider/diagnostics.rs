//! Bounded rehearsal diagnostics. Every string is a local enum serialization;
//! Upstream bodies, messages, IDs and headers are never retained. Codes and
//! parameter names are mapped to exact local allowlists; unknown values disappear.
use crate::engine::{ProviderEndReason, ProviderError, Route};
use serde::Serialize;
use serde_json::Value;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticStage {
    Handshake,
    Startup,
    Streaming,
    Closing,
    Complete,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCategory {
    Confirmed,
    HttpRefused,
    Transport,
    Deadline,
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
    SourceResponseDeadline,
    SourceIncomplete,
    ProtocolCapacityLimit,
    LiveClosedRequested,
    LiveExpired,
    LiveContentClosed,
    LiveRemoteHangup,
    LiveConnectionLost,
    UpstreamError,
    UpstreamInvalidRequest,
    UpstreamInvalidEvent,
    UpstreamServerError,
    UpstreamTranscription,
    UpstreamAudioUnintelligible,
    UnsupportedEvent,
    InvalidJson,
    MissingSessionId,
    MissingSessionType,
    MissingModel,
    MissingExpiry,
    MissingAudioFormat,
    MissingLanguages,
    MissingUsage,
    StartupSchema,
    EventSchema,
    CloseUnconfirmed,
}
#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticEvent {
    None,
    Other,
    Error,
    SessionCreated,
    SessionStarted,
    SessionUpdated,
    SessionClosed,
    AudioDelta,
    TranscriptDelta,
    TranscriptionCompleted,
    AudioCommitted,
    ItemNotice,
    ConversationCreated,
    UsageUpdated,
    CommentaryAppended,
    Delegation,
    Info,
    RateLimits,
    SpeechStarted,
    SpeechStopped,
    TranscriptionFailed,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticCode {
    #[default]
    None,
    Other,
    InvalidEvent,
    InvalidValue,
    InvalidType,
    UnknownParameter,
    UnsupportedParameter,
    UnsupportedValue,
    ModelNotFound,
    RateLimitExceeded,
    InsufficientQuota,
    ServerError,
    AudioUnintelligible,
    InputAudioBufferCommitEmpty,
}

#[derive(Clone, Copy, Debug, Default, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticParameter {
    #[default]
    None,
    Other,
    SessionType,
    AudioFormat,
    AudioRate,
    Transcription,
    TranscriptionModel,
    TranscriptionLanguages,
    TranscriptionDelay,
    NoiseReduction,
    TurnDetection,
    TurnDetectionType,
    TurnDetectionThreshold,
    TurnDetectionPrefixPadding,
    TurnDetectionSilenceDuration,
}

/// Exact matching only. These labels describe an error's reported field/code;
/// they neither establish causation nor change protocol admission.
pub(super) fn upstream_details(value: &Value) -> (DiagnosticCode, DiagnosticParameter) {
    use DiagnosticCode as C;
    use DiagnosticParameter as P;
    if !matches!(
        value["type"].as_str(),
        Some("error" | "conversation.item.input_audio_transcription.failed")
    ) {
        return (C::None, P::None);
    }
    let code = match value["error"].get("code") {
        None | Some(Value::Null) => C::None,
        Some(value) => match value.as_str() {
            Some("invalid_event") => C::InvalidEvent,
            Some("invalid_value") => C::InvalidValue,
            Some("invalid_type") => C::InvalidType,
            Some("unknown_parameter") => C::UnknownParameter,
            Some("unsupported_parameter") => C::UnsupportedParameter,
            Some("unsupported_value") => C::UnsupportedValue,
            Some("model_not_found") => C::ModelNotFound,
            Some("rate_limit_exceeded") => C::RateLimitExceeded,
            Some("insufficient_quota") => C::InsufficientQuota,
            Some("server_error") => C::ServerError,
            Some("audio_unintelligible") => C::AudioUnintelligible,
            Some("input_audio_buffer_commit_empty") => C::InputAudioBufferCommitEmpty,
            _ => C::Other,
        },
    };
    let parameter = match value["error"].get("param") {
        None | Some(Value::Null) => P::None,
        Some(value) => match value.as_str() {
            Some("session.type") => P::SessionType,
            Some("session.audio.input.format") => P::AudioFormat,
            Some("session.audio.input.format.rate") => P::AudioRate,
            Some("session.audio.input.transcription") => P::Transcription,
            Some("session.audio.input.transcription.model") => P::TranscriptionModel,
            Some("session.audio.input.transcription.languages") => P::TranscriptionLanguages,
            Some("session.audio.input.transcription.delay") => P::TranscriptionDelay,
            Some("session.audio.input.noise_reduction") => P::NoiseReduction,
            Some("session.audio.input.turn_detection") => P::TurnDetection,
            Some("session.audio.input.turn_detection.type") => P::TurnDetectionType,
            Some("session.audio.input.turn_detection.threshold") => P::TurnDetectionThreshold,
            Some("session.audio.input.turn_detection.prefix_padding_ms") => {
                P::TurnDetectionPrefixPadding
            }
            Some("session.audio.input.turn_detection.silence_duration_ms") => {
                P::TurnDetectionSilenceDuration
            }
            _ => P::Other,
        },
    };
    (code, parameter)
}

/// One terminal record per reserved stream, at most sixteen. This is diagnostic
/// metadata, not a billing receipt or authority to reopen an uncertain session.
#[derive(Clone, Debug, Serialize)]
pub struct ProviderDiagnostic {
    pub(super) route: Route,
    pub(super) stage: DiagnosticStage,
    pub(super) category: DiagnosticCategory,
    pub(super) event_type: DiagnosticEvent,
    pub(super) upstream_code: DiagnosticCode,
    pub(super) upstream_parameter: DiagnosticParameter,
    pub(super) http_status: Option<u16>,
    pub(super) ready: bool,
    pub(super) closed_confirmed: bool,
}

pub(super) fn error_category(error: ProviderError) -> DiagnosticCategory {
    match error {
        ProviderError::Deadline => DiagnosticCategory::Deadline,
        ProviderError::Budget => DiagnosticCategory::Budget,
        ProviderError::TextLimit => DiagnosticCategory::TextLimit,
        ProviderError::StreamLimit => DiagnosticCategory::StreamLimit,
        ProviderError::InputAudioLimit => DiagnosticCategory::InputAudioLimit,
        ProviderError::OutputAudioLimit => DiagnosticCategory::OutputAudioLimit,
        ProviderError::InputPacing => DiagnosticCategory::InputPacing,
        ProviderError::SpeechBoundaryLimit => DiagnosticCategory::SpeechBoundaryLimit,
        ProviderError::SourceLedgerLimit => DiagnosticCategory::SourceLedgerLimit,
        ProviderError::TranscriptItemLimit => DiagnosticCategory::TranscriptItemLimit,
        ProviderError::UsageMismatch | ProviderError::AsrUsageMismatch { .. } => {
            DiagnosticCategory::UsageMismatch
        }
        ProviderError::SourceResponseDeadline => DiagnosticCategory::SourceResponseDeadline,
        ProviderError::SourceIncomplete => DiagnosticCategory::SourceIncomplete,
        ProviderError::ProtocolCapacityLimit => DiagnosticCategory::ProtocolCapacityLimit,
        ProviderError::SessionEnded(reason) => match reason {
            ProviderEndReason::CloseRequested => DiagnosticCategory::LiveClosedRequested,
            ProviderEndReason::Expired => DiagnosticCategory::LiveExpired,
            ProviderEndReason::Content => DiagnosticCategory::LiveContentClosed,
            ProviderEndReason::RemoteHangup => DiagnosticCategory::LiveRemoteHangup,
            ProviderEndReason::ConnectionLost => DiagnosticCategory::LiveConnectionLost,
        },
        ProviderError::Protocol => DiagnosticCategory::EventSchema,
        ProviderError::Unavailable
        | ProviderError::Disabled
        | ProviderError::ConnectionLost
        | ProviderError::InputInterrupted => DiagnosticCategory::Transport,
    }
}

/// Classifies a rejected event using only key presence and a closed event map.
/// It does not participate in protocol acceptance or change any guard.
pub(super) fn rejected_event(
    route: Route,
    ready: bool,
    value: &Value,
) -> (DiagnosticEvent, DiagnosticCategory) {
    use DiagnosticCategory as C;
    use DiagnosticEvent as E;
    let event = match value["type"].as_str() {
        Some("error") => E::Error,
        Some("session.created") => E::SessionCreated,
        Some("session.started") => E::SessionStarted,
        Some("session.updated") => E::SessionUpdated,
        Some("session.closed") => E::SessionClosed,
        Some("session.output_audio.delta") => E::AudioDelta,
        Some(
            "session.input_transcript.delta"
            | "session.output_transcript.delta"
            | "conversation.item.input_audio_transcription.delta",
        ) => E::TranscriptDelta,
        Some("conversation.item.input_audio_transcription.completed") => E::TranscriptionCompleted,
        Some("input_audio_buffer.committed") => E::AudioCommitted,
        Some(
            "conversation.item.added" | "conversation.item.created" | "conversation.item.done",
        ) => E::ItemNotice,
        Some("conversation.created") => E::ConversationCreated,
        Some("session.usage.updated") => E::UsageUpdated,
        Some("session.commentary.appended") => E::CommentaryAppended,
        Some("session.delegation.created" | "session.delegation.cancelled") => E::Delegation,
        Some("info") => E::Info,
        Some("rate_limits.updated") => E::RateLimits,
        Some("input_audio_buffer.speech_started") => E::SpeechStarted,
        Some("input_audio_buffer.speech_stopped") => E::SpeechStopped,
        Some("conversation.item.input_audio_transcription.failed") => E::TranscriptionFailed,
        _ => E::Other,
    };
    let category = if matches!(event, E::Error | E::TranscriptionFailed) {
        upstream_category(&value["error"])
    } else if event == E::Other {
        C::UnsupportedEvent
    } else if matches!(
        event,
        E::SessionCreated | E::SessionStarted | E::SessionUpdated
    ) {
        let s = &value["session"];
        if s.get("id").is_none_or(Value::is_null) {
            C::MissingSessionId
        } else if route != Route::Live && s.get("type").is_none_or(Value::is_null) {
            C::MissingSessionType
        } else if route != Route::Transcription && s.get("model").is_none_or(Value::is_null) {
            C::MissingModel
        } else if (route != Route::Transcription && s.get("expires_at").is_none())
            || s.get("expires_at").is_some_and(Value::is_null)
        {
            C::MissingExpiry
        } else if event != E::SessionCreated
            && route == Route::Transcription
            && s.pointer("/audio/input/transcription/model")
                .is_none_or(Value::is_null)
        {
            C::MissingModel
        } else if event != E::SessionCreated
            && route == Route::Transcription
            && s.pointer("/audio/input/transcription/languages")
                .is_none_or(Value::is_null)
        {
            C::MissingLanguages
        } else if event != E::SessionCreated
            && route != Route::Translation
            && s.pointer(if route == Route::Live {
                "/audio/format"
            } else {
                "/audio/input/format"
            })
            .is_none_or(Value::is_null)
        {
            C::MissingAudioFormat
        } else {
            C::StartupSchema
        }
    } else if matches!(event, E::UsageUpdated | E::TranscriptionCompleted)
        || (event == E::SessionClosed && route == Route::Live)
    {
        if value.get("usage").is_none_or(Value::is_null) {
            C::MissingUsage
        } else {
            C::EventSchema
        }
    } else if !ready {
        C::StartupSchema
    } else {
        C::EventSchema
    };
    (event, category)
}

/// Exact examples/types documented in the Realtime server-event reference.
/// No heuristic matching, arbitrary value serialization or automatic retry.
fn upstream_category(error: &Value) -> DiagnosticCategory {
    use DiagnosticCategory as C;
    let kind = error["type"].as_str();
    let code = error.get("code");
    if !error.is_object()
        || kind.is_none_or(|s| s.len() > 128)
        || code.is_some_and(|c| !c.is_null() && c.as_str().is_none_or(|s| s.len() > 128))
    {
        return C::UpstreamError;
    }
    match (kind, code.and_then(Value::as_str)) {
        (Some("invalid_request_error"), Some("invalid_event")) => C::UpstreamInvalidEvent,
        (Some("transcription_error"), Some("audio_unintelligible")) => {
            C::UpstreamAudioUnintelligible
        }
        (Some("invalid_request_error"), _) => C::UpstreamInvalidRequest,
        (Some("server_error"), _) => C::UpstreamServerError,
        (Some("transcription_error"), _) => C::UpstreamTranscription,
        _ => C::UpstreamError,
    }
}
