//! Fixed, memory-only observations for the authenticated operator after Stop.
//! No provider bodies, text, audio, device identities or route IDs are retained.
use super::*;

// JSON-safe integer and small enough to pack with an eight-bit Stop code.
const MAX_ELAPSED_SECONDS: u64 = (1_u64 << 53) - 1;

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
#[repr(u8)]
pub enum StopCode {
    Unknown = 1,
    OperatorStop = 2,
    OperatorDisconnected = 3,
    Deadline = 4,
    Backpressure = 5,
    InvalidFrame = 6,
    ProcessShutdown = 7,
}
impl StopCode {
    pub(super) fn from_raw(code: &str) -> Self {
        match code {
            "operator_stop" => Self::OperatorStop,
            "operator_disconnected" => Self::OperatorDisconnected,
            "deadline" => Self::Deadline,
            "backpressure" => Self::Backpressure,
            "invalid_frame" => Self::InvalidFrame,
            "process_shutdown" => Self::ProcessShutdown,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct StopDiagnostic {
    pub code: StopCode,
    pub elapsed_seconds: u64,
}

/// The first reason and Stop latch are one atomic operation. Recording it never
/// waits for the state mutex, and a concurrent read cannot see a missing reason
/// after observing the latch. Zero alone means not stopped.
pub(super) struct StopLatch(AtomicU64);
impl StopLatch {
    pub(super) fn new() -> Self {
        Self(AtomicU64::new(0))
    }
    pub(super) fn load(&self, ordering: Ordering) -> bool {
        self.0.load(ordering) != 0
    }
    pub(super) fn try_stop(&self, code: StopCode, elapsed_seconds: u64) -> bool {
        let value = (elapsed_seconds.min(MAX_ELAPSED_SECONDS) << 8) | code as u64;
        self.0
            .compare_exchange(0, value, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub(super) fn snapshot(&self) -> Option<StopDiagnostic> {
        let value = self.0.load(Ordering::Acquire);
        if value == 0 {
            return None;
        }
        let code = match value & 0xff {
            2 => StopCode::OperatorStop,
            3 => StopCode::OperatorDisconnected,
            4 => StopCode::Deadline,
            5 => StopCode::Backpressure,
            6 => StopCode::InvalidFrame,
            7 => StopCode::ProcessShutdown,
            _ => StopCode::Unknown,
        };
        Some(StopDiagnostic {
            code,
            elapsed_seconds: value >> 8,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum IssueCode {
    Unknown,
    ProviderUnavailable,
    ProviderConnectionLost,
    SourceInputInterrupted,
    ProviderProtocol,
    ProviderClosed,
    LiveClosedRequested,
    LiveExpired,
    LiveContentClosed,
    LiveRemoteHangup,
    LiveConnectionLost,
    BudgetExhausted,
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
    Deadline,
    SourceGap,
    SequenceGap,
    Backpressure,
}
impl IssueCode {
    pub(super) fn from_raw(code: &str) -> Self {
        match code {
            "provider_unavailable" => Self::ProviderUnavailable,
            "provider_connection_lost" => Self::ProviderConnectionLost,
            "source_input_interrupted" => Self::SourceInputInterrupted,
            "provider_protocol" => Self::ProviderProtocol,
            "provider_closed" => Self::ProviderClosed,
            "live_closed_requested" => Self::LiveClosedRequested,
            "live_expired" => Self::LiveExpired,
            "live_content_closed" => Self::LiveContentClosed,
            "live_remote_hangup" => Self::LiveRemoteHangup,
            "live_connection_lost" => Self::LiveConnectionLost,
            "budget_exhausted" => Self::BudgetExhausted,
            "text_limit" => Self::TextLimit,
            "stream_limit" => Self::StreamLimit,
            "input_audio_limit" => Self::InputAudioLimit,
            "output_audio_limit" => Self::OutputAudioLimit,
            "input_pacing" => Self::InputPacing,
            "speech_boundary_limit" => Self::SpeechBoundaryLimit,
            "source_ledger_limit" => Self::SourceLedgerLimit,
            "transcript_item_limit" => Self::TranscriptItemLimit,
            "usage_mismatch" => Self::UsageMismatch,
            "source_response_deadline" => Self::SourceResponseDeadline,
            "source_incomplete" => Self::SourceIncomplete,
            "protocol_capacity_limit" => Self::ProtocolCapacityLimit,
            "deadline" => Self::Deadline,
            "source_gap" => Self::SourceGap,
            "sequence_gap" => Self::SequenceGap,
            "backpressure" => Self::Backpressure,
            _ => Self::Unknown,
        }
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct AsrUsageDiagnostic {
    pub submitted_ms: u32,
    pub reported_ms: Option<u64>,
    pub allowed_ms: u32,
}
impl AsrUsageDiagnostic {
    fn from_error(error: ProviderError) -> Option<Self> {
        let ProviderError::AsrUsageMismatch {
            submitted_ms,
            reported_ms,
            allowed_ms,
        } = error
        else {
            return None;
        };
        // This fixed observation is not upstream JSON or authority to relax a
        // usage guard. Invalid internal bounds must not widen the read model.
        if submitted_ms > 120_000
            || allowed_ms != submitted_ms + 1_000
            || reported_ms.is_some_and(|reported| reported <= u64::from(allowed_ms))
        {
            return None;
        }
        Some(Self {
            submitted_ms,
            reported_ms: reported_ms.filter(|value| *value <= MAX_ELAPSED_SECONDS),
            allowed_ms,
        })
    }
}

#[derive(Clone, Copy, Debug, Serialize, PartialEq, Eq)]
pub struct IssueDiagnostic {
    pub code: IssueCode,
    pub source: Source,
    pub route: Option<Route>,
    pub elapsed_seconds: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub asr_usage: Option<AsrUsageDiagnostic>,
}

#[derive(Clone, Debug, Serialize, PartialEq, Eq)]
pub struct DiagnosticSnapshot {
    pub version: u8,
    pub generation: u64,
    pub stopped: bool,
    pub stop: Option<StopDiagnostic>,
    pub last_issue: Option<IssueDiagnostic>,
}

impl Pilot {
    pub(super) fn elapsed_seconds(&self) -> u64 {
        self.started_at.elapsed().as_secs().min(MAX_ELAPSED_SECONDS)
    }

    fn operator_observation(
        &self,
        headers: &HeaderMap,
    ) -> Result<std::sync::MutexGuard<'_, Memory>, ApiError> {
        let session_id = self.authorize(headers, Role::Operator, false)?;
        let memory = self.memory.lock().expect("pilot state");
        if memory.operator.as_ref().is_none_or(|session| {
            session.id != session_id || !session.matches(auth::bearer(headers))
        }) {
            return Err(ApiError::Unauthorized);
        }
        if self.expired() {
            drop(memory);
            self.stop("deadline");
            return Err(ApiError::Unauthorized);
        }
        Ok(memory)
    }

    /// Meeting-local reservations, closure observations and the unavailable
    /// invoice are distinct. No provider calls are made when reading a receipt.
    pub(crate) fn stop_receipt(&self, headers: &HeaderMap) -> Result<Value, ApiError> {
        let _memory = self.operator_observation(headers)?;
        let generation = self.stopped_generation().ok_or(ApiError::Conflict)?;
        let evidence = self.backend.stop_evidence();
        Ok(json!({
            "version":1, "session_generation":self.initial_generation, "generation":generation,
            "provider_activity":evidence.activity, "reserved_microusd":evidence.reserved_microusd,
            "invoice":"unavailable"
        }))
    }

    /// Readback has the same identity and deadline checks as other operator
    /// resources. Stopped state permits observation only, never renewed work.
    pub fn diagnostics(&self, headers: &HeaderMap) -> Result<DiagnosticSnapshot, ApiError> {
        let memory = self.operator_observation(headers)?;
        let stop = self.stopped.snapshot();
        Ok(DiagnosticSnapshot {
            version: 1,
            generation: self.generation(),
            stopped: stop.is_some(),
            stop,
            last_issue: memory.last_issue,
        })
    }

    /// Caller owns Memory and has observed this source failure before Stop.
    /// Capture the route kind before its identity is removed; unknown raw
    /// strings are discarded rather than serialized into the read model.
    pub(super) fn record_issue(
        &self,
        memory: &mut Memory,
        source: Source,
        route: Option<Route>,
        code: &str,
    ) -> bool {
        self.record_issue_details(memory, source, route, code, None)
    }

    pub(super) fn record_provider_issue(
        &self,
        memory: &mut Memory,
        source: Source,
        route: Option<Route>,
        error: ProviderError,
    ) -> bool {
        self.record_issue_details(memory, source, route, error.code(), Some(error))
    }

    fn record_issue_details(
        &self,
        memory: &mut Memory,
        source: Source,
        route: Option<Route>,
        code: &str,
        error: Option<ProviderError>,
    ) -> bool {
        // Sample before checking Stop so a delayed callback cannot acquire a
        // newer timestamp after it has observed the terminal latch. These
        // independently sampled whole seconds do not establish event order.
        let elapsed_seconds = self.elapsed_seconds();
        if !self.is_stopped() {
            memory.last_issue = Some(IssueDiagnostic {
                code: IssueCode::from_raw(code),
                source,
                route,
                elapsed_seconds,
                asr_usage: error
                    .filter(|_| route == Some(Route::Transcription))
                    .and_then(AsrUsageDiagnostic::from_error),
            });
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests;
