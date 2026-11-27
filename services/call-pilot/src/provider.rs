//! Explicit finite, vendor-bound transports for the supervised local pilot. This module
//! never discovers keys, endpoints, models, devices, tools, or account settings.
//! Local reservations are conservative counters, not an account billing cap.
const GEMINI_MAX_DURATION_SECONDS: u64 = 600;
pub const ASR_PROTOCOL_PROFILE: &str = "manual_local_boundaries_v1";
mod asr;
mod audio;
mod budget;
mod diagnostics;
mod gemini;
mod guard;
pub(crate) mod local_vad;
mod protocol;
#[cfg(test)]
mod tests;
mod transport;

pub use diagnostics::{
    DiagnosticCategory, DiagnosticCode, DiagnosticEvent, DiagnosticParameter, DiagnosticStage,
    ProviderDiagnostic,
};

use crate::engine::{
    Language, LiveMode, PreparedAudio, ProviderBackend, ProviderError, ProviderFuture, Route,
    StreamHandle,
};
use std::{
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::watch;

/// Intentionally neither Debug nor Serialize: the secret enters only from the
/// separately approved launcher. Zero fields are ambient defaults.
pub struct ProviderConfig {
    pub api_key: String,
    pub allowed_routes: Vec<Route>,
    pub allowed_live_modes: Vec<LiveMode>,
    pub allow_tts: bool,
    pub total_duration_seconds: u64,
    pub route_duration_seconds: u64,
    pub max_sessions: u32,
    pub max_tts_requests: u32,
    pub max_input_audio_seconds: u64,
    pub max_output_audio_seconds: u64,
    pub max_text_characters: u64,
    pub approved_budget_microusd: u64,
}

pub type OpenAiBackend = FiniteBackend;

pub struct FiniteBackend {
    shared: Arc<Shared>,
}
use crate::budget::ProviderVendor;

/// Do not route a recognizable foreign credential to the selected host.
/// Key ownership comes from explicit activation, never from prefix inference.
pub(crate) fn valid_key(vendor: ProviderVendor, key: &str) -> bool {
    (16..=4096).contains(&key.len())
        && key.bytes().all(|b| b.is_ascii_graphic())
        && match vendor {
            ProviderVendor::OpenAi => !key.starts_with("AIza") && !key.starts_with("AQ."),
            ProviderVendor::Gemini => !key.starts_with("sk-"),
        }
}

struct Shared {
    vendor: ProviderVendor,
    #[cfg(test)]
    gemini_peers: Option<gemini::FixturePeers>,
    config: ProviderConfig,
    deadline: Instant,
    state: Mutex<Ledger>,
    stop: watch::Sender<bool>,
    tasks: Mutex<Vec<tokio::task::JoinHandle<()>>>,
    diagnostics: Mutex<Vec<ProviderDiagnostic>>,
    revocation: guard::ExternalRevocation,
    admission: guard::ExternalRevocation,
    input_idle_grace: Duration,
    multilingual: bool,
}
/// A desktop approval shares one paid window. Legacy profiles grant each
/// successful admission an independent route duration inside the process limit.
#[derive(Clone, Copy, Default)]
enum StreamWindow {
    #[default]
    Independent,
    AwaitingFirstStream,
    MeetingEndsAt(Instant),
}

#[derive(Default)]
struct Ledger {
    window: StreamWindow,
    sessions: u32,
    tts: u32,
    reserved: u64,
    input_samples: u64,
    output_samples_24k: u64,
    text: u64,
    uncertain: bool,
    tts_active: bool,
}

/// A cancelled shutdown must abort the handles it already removed from the
/// registry. Dropping a bare JoinHandle would detach still-owned work.
struct ShutdownTasks<'a> {
    shared: &'a Shared,
    tasks: Vec<tokio::task::JoinHandle<()>>,
    completed: bool,
}
impl Drop for ShutdownTasks<'_> {
    fn drop(&mut self) {
        if !self.completed {
            self.shared.poison();
            for task in &self.tasks {
                task.abort();
            }
        }
    }
}

impl FiniteBackend {
    pub fn new(config: ProviderConfig) -> Result<Self, ProviderError> {
        Self::new_with_revocation(config, Vec::new())
    }
    /// Parent cancellation is checked directly by transport polling. It does
    /// not depend on a supervisor being scheduled to copy a Stop signal.
    pub fn new_with_revocation(
        config: ProviderConfig,
        parents: Vec<watch::Receiver<bool>>,
    ) -> Result<Self, ProviderError> {
        Self::new_with_controls(config, parents, None)
    }
    /// The admission fence prevents each not-yet-ready stream from starting.
    /// Unlike Stop parents it does not cancel already-admitted source draining.
    pub fn new_with_controls(
        config: ProviderConfig,
        parents: Vec<watch::Receiver<bool>>,
        admission_fence: Option<watch::Receiver<bool>>,
    ) -> Result<Self, ProviderError> {
        Self::initialize(
            config,
            parents,
            admission_fence,
            ProviderVendor::OpenAi,
            StreamWindow::Independent,
        )
    }

    pub(crate) fn for_vendor(
        config: ProviderConfig,
        vendor: ProviderVendor,
    ) -> Result<Self, ProviderError> {
        Self::initialize(config, Vec::new(), None, vendor, StreamWindow::Independent)
    }

    pub(crate) fn for_desktop_meeting(config: ProviderConfig) -> Result<Self, ProviderError> {
        Self::initialize(
            config,
            Vec::new(),
            None,
            ProviderVendor::OpenAi,
            StreamWindow::AwaitingFirstStream,
        )
    }

    fn initialize(
        config: ProviderConfig,
        parents: Vec<watch::Receiver<bool>>,
        admission_fence: Option<watch::Receiver<bool>>,
        vendor: ProviderVendor,
        window: StreamWindow,
    ) -> Result<Self, ProviderError> {
        if vendor == ProviderVendor::Gemini
            && (config.total_duration_seconds > GEMINI_MAX_DURATION_SECONDS
                || config.route_duration_seconds > GEMINI_MAX_DURATION_SECONDS
                || config.allow_tts
                || config.max_tts_requests != 0
                || config.allowed_routes.contains(&Route::Translation)
                || config.allowed_live_modes.contains(&LiveMode::Assistant))
        {
            return Err(ProviderError::Protocol);
        }
        let multilingual = admission_fence.is_some();
        let revocation = guard::ExternalRevocation::new(parents)?;
        let input_idle_grace = Duration::from_secs(if admission_fence.is_some() { 15 } else { 2 });
        let admission = guard::ExternalRevocation::new(admission_fence.into_iter().collect())?;
        if !(30..=7200).contains(&config.total_duration_seconds)
            || !(20..=3600).contains(&config.route_duration_seconds)
            || !(1..=16).contains(&config.max_sessions)
            || config.max_tts_requests > 8
            || config.max_input_audio_seconds == 0
            || config.max_input_audio_seconds > 115_200
            || config.max_output_audio_seconds == 0
            || config.max_output_audio_seconds > 115_200
            || config.max_text_characters == 0
            || config.max_text_characters > 1_000_000
            || config.approved_budget_microusd == 0
            || config.approved_budget_microusd > 1_000_000_000
            || config.allowed_routes.len() > 3
            || config.allowed_live_modes.len() > 2
            || (config.allowed_routes.contains(&Route::Live)
                == config.allowed_live_modes.is_empty())
            || config
                .allowed_live_modes
                .iter()
                .enumerate()
                .any(|(i, mode)| config.allowed_live_modes[..i].contains(mode))
            || !valid_key(vendor, &config.api_key)
            || config
                .allowed_routes
                .iter()
                .enumerate()
                .any(|(i, r)| config.allowed_routes[..i].contains(r))
            || (config.allowed_routes.is_empty() && !config.allow_tts)
        {
            return Err(ProviderError::Protocol);
        }
        let deadline = Instant::now() + Duration::from_secs(config.total_duration_seconds);
        let (stop, _) = watch::channel(false);
        Ok(Self {
            shared: Arc::new(Shared {
                vendor,
                #[cfg(test)]
                gemini_peers: None,
                config,
                deadline,
                state: Mutex::new(Ledger {
                    window,
                    ..Ledger::default()
                }),
                stop,
                tasks: Mutex::new(Vec::new()),
                diagnostics: Mutex::new(Vec::new()),
                revocation,
                admission,
                input_idle_grace,
                multilingual,
            }),
        })
    }
    /// Closed-schema metadata only. Read after Closed/shutdown to include the
    /// terminal record; no transcript, provider ID, raw error or secret is stored.
    pub fn diagnostics(&self) -> Vec<ProviderDiagnostic> {
        self.shared
            .diagnostics
            .lock()
            .map(|d| d.clone())
            .unwrap_or_default()
    }
    /// Stop all routes independently of full queues and join their bounded
    /// cleanup. A timeout aborts only owned tasks and permanently poisons work.
    /// Actual transport close observation, independent of source completeness
    /// and accounting uncertainty. Call only after local retirement completes.
    pub(crate) fn closure_confirmed(&self) -> bool {
        let Ok(ledger) = self.shared.state.lock() else {
            return false;
        };
        self.shared.all_streams_closed(ledger.sessions)
    }
    pub async fn shutdown(&self) {
        let _ = self.retire().await;
    }
    /// Join or abort-and-join every owned transport. This proves local resource
    /// retirement only, never remote final usage or invoice reconciliation.
    pub(crate) async fn retire(&self) -> bool {
        self.shared.stop.send_replace(true);
        let tasks = match self.shared.tasks.lock() {
            Ok(mut tasks) => std::mem::take(&mut *tasks),
            Err(_) => return false,
        };
        let mut owned = ShutdownTasks {
            shared: &self.shared,
            tasks,
            completed: false,
        };
        let deadline = Instant::now() + Duration::from_secs(11);
        for task in &mut owned.tasks {
            match transport::deadline(deadline, &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => self.shared.poison(),
                Err(_) => {
                    self.shared.poison();
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        owned.completed = true;
        true
    }
}
impl Drop for FiniteBackend {
    fn drop(&mut self) {
        self.shared.stop.send_replace(true);
    }
}
impl ProviderBackend for FiniteBackend {
    fn vendor(&self) -> ProviderVendor {
        self.shared.vendor
    }
    fn revoke(&self) {
        self.shared.stop.send_replace(true);
    }
    fn stop_evidence(&self) -> crate::engine::ProviderStopEvidence {
        use crate::engine::{ProviderActivity, ProviderStopEvidence};
        let Ok(ledger) = self.shared.state.lock() else {
            return ProviderStopEvidence::default();
        };
        if !*self.shared.stop.borrow() {
            return ProviderStopEvidence::default();
        }
        // Reservation checks this same ledger and fence before registering
        // paid work. Counts never reset. An in-flight or missing observation,
        // or any HTTP synthesis request, cannot establish all-stream closure.
        let activity = if ledger.sessions == 0 && ledger.tts == 0 {
            ProviderActivity::NoneAdmitted
        } else if ledger.tts == 0 && self.shared.all_streams_closed(ledger.sessions) {
            ProviderActivity::CloseObserved
        } else {
            ProviderActivity::Unconfirmed
        };
        ProviderStopEvidence {
            activity,
            reserved_microusd: Some(ledger.reserved),
        }
    }
    fn enabled(&self) -> bool {
        self.shared.available()
    }
    fn supports(&self, route: Route) -> bool {
        // Stable configured capability; the fresh budget quote and atomic
        // reservation decide affordability for each explicit action.
        self.shared.available() && self.shared.config.allowed_routes.contains(&route)
    }
    fn supports_tts(&self) -> bool {
        self.shared.available() && self.shared.config.allow_tts
    }
    fn supports_live_mode(&self, mode: LiveMode) -> bool {
        self.supports(Route::Live) && self.shared.config.allowed_live_modes.contains(&mode)
    }
    fn budget_snapshot(&self) -> crate::budget::BudgetSnapshot {
        self.shared.budget_snapshot()
    }
    fn open(&self, route: Route, language: Language) -> ProviderFuture<StreamHandle> {
        self.open_with_mode(route, language, None)
    }
    fn open_with_mode(
        &self,
        route: Route,
        language: Language,
        mode: Option<LiveMode>,
    ) -> ProviderFuture<StreamHandle> {
        let shared = self.shared.clone();
        Box::pin(async move {
            match shared.vendor {
                ProviderVendor::OpenAi => transport::open(shared, route, language, mode).await,
                ProviderVendor::Gemini => {
                    gemini::open(shared, route, language, mode, None, None, None).await
                }
            }
        })
    }
    fn open_interpretation(
        &self,
        language: Language,
        policy: crate::engine::Interpretation,
    ) -> ProviderFuture<StreamHandle> {
        self.open_interpretation_context(language, policy, Default::default())
    }
    fn open_interpretation_context(
        &self,
        language: Language,
        policy: crate::engine::Interpretation,
        context: crate::interpretation_context::InterpretationContext,
    ) -> ProviderFuture<StreamHandle> {
        let shared = self.shared.clone();
        if shared.vendor == ProviderVendor::Gemini {
            return Box::pin(async { Err(ProviderError::Unavailable) });
        }
        Box::pin(async move {
            transport::open_extended(
                shared,
                Route::Live,
                language,
                Some(LiveMode::Interpretation),
                None,
                Some(policy),
                Some(context),
            )
            .await
        })
    }
    fn open_transcription(
        &self,
        language: Language,
        hints: crate::engine::SpeechHints,
    ) -> ProviderFuture<StreamHandle> {
        let shared = self.shared.clone();
        Box::pin(async move {
            match shared.vendor {
                ProviderVendor::OpenAi => {
                    transport::open_configured(
                        shared,
                        Route::Transcription,
                        language,
                        None,
                        Some(hints),
                    )
                    .await
                }
                ProviderVendor::Gemini => {
                    gemini::open(
                        shared,
                        Route::Transcription,
                        language,
                        None,
                        Some(hints),
                        None,
                        None,
                    )
                    .await
                }
            }
        })
    }
    fn synthesize(&self, text: String, language: Language) -> ProviderFuture<PreparedAudio> {
        let shared = self.shared.clone();
        if shared.vendor == ProviderVendor::Gemini {
            return Box::pin(async { Err(ProviderError::Unavailable) });
        }
        Box::pin(async move { transport::synthesize(shared, text, language).await })
    }
}
impl Shared {
    fn all_streams_closed(&self, sessions: u32) -> bool {
        self.diagnostics.lock().is_ok_and(|records| {
            records.len() == sessions as usize && records.iter().all(|d| d.closed_confirmed)
        })
    }
    fn record_diagnostic(&self, diagnostic: ProviderDiagnostic) {
        if let Ok(mut records) = self.diagnostics.lock()
            && records.len() < 16
        {
            records.push(diagnostic);
        }
    }
    fn spawn_owned(
        &self,
        future: impl std::future::Future<Output = ()> + Send + 'static,
    ) -> Result<(), ProviderError> {
        // Registration and shutdown take the same registry lock. A task is
        // either joined by shutdown or refused before its first poll.
        let mut tasks = self.tasks.lock().map_err(|_| ProviderError::Unavailable)?;
        let stopped = *self.stop.borrow() || self.revocation.revoked() || self.admission.revoked();
        if stopped {
            return Err(ProviderError::Deadline);
        }
        tasks.retain(|task| !task.is_finished());
        if tasks.len() >= self.config.max_sessions as usize {
            return Err(ProviderError::StreamLimit);
        }
        tasks.push(tokio::spawn(future));
        Ok(())
    }
    fn window_end(&self, window: StreamWindow, now: Instant) -> Instant {
        match window {
            StreamWindow::Independent => self.deadline,
            StreamWindow::AwaitingFirstStream => self
                .deadline
                .min(now + Duration::from_secs(self.config.route_duration_seconds)),
            StreamWindow::MeetingEndsAt(end) => end,
        }
    }
    fn route_seconds(&self, remaining: Duration) -> u64 {
        self.config.route_duration_seconds.min(remaining.as_secs())
    }
    fn available(&self) -> bool {
        self.state.lock().is_ok_and(|state| {
            let now = Instant::now();
            !state.uncertain
                && !*self.stop.borrow()
                && !self.revocation.revoked()
                && !self.admission.revoked()
                && now + Duration::from_secs(10) < self.window_end(state.window, now)
        })
    }
    fn poison(&self) {
        if let Ok(mut state) = self.state.lock() {
            state.uncertain = true;
        }
        self.stop.send_replace(true);
    }
    fn reserve(
        self: &Arc<Self>,
        route: Option<Route>,
        text: usize,
    ) -> Result<Reservation, ProviderError> {
        let mut state = self.state.lock().map_err(|_| ProviderError::Unavailable)?;
        self.reserve_locked(&mut state, route, text, Instant::now())
    }
    fn reserve_locked(
        self: &Arc<Self>,
        state: &mut Ledger,
        route: Option<Route>,
        text: usize,
        now: Instant,
    ) -> Result<Reservation, ProviderError> {
        let end = self.window_end(state.window, now);
        if state.uncertain
            || *self.stop.borrow()
            || self.revocation.revoked()
            || self.admission.revoked()
            || now + Duration::from_secs(10) >= end
        {
            return Err(ProviderError::Unavailable);
        }
        let seconds = self.route_seconds(end.saturating_duration_since(now));
        let cost = match route {
            Some(route) => {
                if !self.config.allowed_routes.contains(&route)
                    || state.sessions >= self.config.max_sessions
                {
                    return Err(ProviderError::StreamLimit);
                }
                self.vendor
                    .stream_cost(route, seconds)
                    .ok_or(ProviderError::Budget)?
            }
            None => {
                if !self.config.allow_tts
                    || state.tts_active
                    || state.tts >= self.config.max_tts_requests
                {
                    return Err(ProviderError::StreamLimit);
                }
                // $30 / million input characters. UTF-8 bytes are a
                // conservative character bound; reserve once, never refund.
                // This remains a local allowance, not an account billing cap.
                protocol::tts_cost(text)?
            }
        };
        let total = state
            .reserved
            .checked_add(cost)
            .ok_or(ProviderError::Budget)?;
        if total > self.config.approved_budget_microusd {
            return Err(ProviderError::Budget);
        }
        state.reserved = total;
        if route.is_some() {
            state.sessions += 1;
            if matches!(state.window, StreamWindow::AwaitingFirstStream) {
                state.window = StreamWindow::MeetingEndsAt(end);
            }
        } else {
            state.tts += 1;
            state.tts_active = true;
        }
        Ok(Reservation {
            shared: self.clone(),
            completed: false,
            tts: route.is_none(),
            seconds,
            // Authorize only the whole seconds this route actually reserved.
            // A delayed first transport poll cannot extend that approval.
            meeting_cutoff: match state.window {
                StreamWindow::MeetingEndsAt(end) => {
                    Some(end.min(now + Duration::from_secs(seconds)))
                }
                _ => None,
            },
        })
    }
    fn input(&self, samples: u64) -> Result<(), ProviderError> {
        let mut s = self.state.lock().map_err(|_| ProviderError::Unavailable)?;
        s.input_samples = s
            .input_samples
            .checked_add(samples)
            .ok_or(ProviderError::InputAudioLimit)?;
        if s.input_samples > self.config.max_input_audio_seconds * 16_000 {
            drop(s);
            self.poison();
            return Err(ProviderError::InputAudioLimit);
        }
        Ok(())
    }
    fn output(&self, bytes: usize, rate: u32) -> Result<(), ProviderError> {
        let mut s = self.state.lock().map_err(|_| ProviderError::Unavailable)?;
        let units = (bytes as u64 / 2) * (24_000 / u64::from(rate));
        // 16 kHz needs an exact 3/2 conversion of sample count.
        let units = if rate == 16_000 {
            (bytes as u64 / 2 * 3).div_ceil(2)
        } else {
            units
        };
        s.output_samples_24k = s
            .output_samples_24k
            .checked_add(units)
            .ok_or(ProviderError::OutputAudioLimit)?;
        if s.output_samples_24k > self.config.max_output_audio_seconds * 24_000 {
            drop(s);
            self.poison();
            return Err(ProviderError::OutputAudioLimit);
        }
        Ok(())
    }
    fn text(&self, text: &str) -> Result<(), ProviderError> {
        self.text_count(text.chars().count())
    }
    fn text_count(&self, count: usize) -> Result<(), ProviderError> {
        let mut s = self.state.lock().map_err(|_| ProviderError::Unavailable)?;
        // This is ingress volume, including ASR delta/final representations and
        // both Live transcripts. It is neither unique speech nor paid tokens.
        // Saturation retains a monotonic counter even if arithmetic overflows;
        // overflow takes the same fail-closed path as an ordinary text limit.
        let next = s.text.checked_add(count as u64);
        s.text = next.unwrap_or(u64::MAX);
        if next.is_some() && s.text <= self.config.max_text_characters {
            return Ok(());
        }
        drop(s);
        self.poison();
        Err(ProviderError::TextLimit)
    }
}
struct Reservation {
    shared: Arc<Shared>,
    completed: bool,
    tts: bool,
    seconds: u64,
    meeting_cutoff: Option<Instant>,
}
impl Reservation {
    fn stream_deadline(&self, transport_started: Instant) -> Instant {
        self.meeting_cutoff.unwrap_or_else(|| {
            (transport_started + Duration::from_secs(self.seconds)).min(self.shared.deadline)
        })
    }
}
impl Drop for Reservation {
    fn drop(&mut self) {
        if !self.completed {
            self.shared.poison();
        }
        if self.tts
            && let Ok(mut s) = self.shared.state.lock()
        {
            s.tts_active = false;
        }
    }
}
