use crate::{
    auth::{self, ApiError, Role, Session, Ticket},
    engine::{
        Language, LiveMode, ProviderBackend, ProviderError, ProviderFailure, Route, StreamEvent,
        StreamHandle, StreamInput,
    },
};
use axum::http::HeaderMap;
use base64::{Engine, engine::general_purpose::STANDARD};
use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    future::Future,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, AtomicU64, Ordering},
    },
    task::Poll,
    time::{Duration, Instant},
};
use tokio::sync::{Semaphore, mpsc, watch};

mod budget;
mod diagnostics;
mod finish;
mod source_drafts;
use diagnostics::StopLatch;
pub use diagnostics::{
    AsrUsageDiagnostic, DiagnosticSnapshot, IssueCode, IssueDiagnostic, StopCode, StopDiagnostic,
};
use finish::FinishState;

#[derive(Clone, Copy, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    RemoteMix,
    #[serde(rename = "self")]
    SelfSource,
}
impl Source {
    fn index(self) -> usize {
        match self {
            Self::RemoteMix => 0,
            Self::SelfSource => 1,
        }
    }
}

#[derive(Clone, Serialize)]
pub struct Preview {
    pub id: String,
    pub generation: u64,
    pub text: String,
    pub language: Language,
    pub digest: String,
    pub audio_path: String,
    pub duration_ms: u64,
    pub auditioned: bool,
    pub state: &'static str,
}
struct Clip {
    view: Preview,
    audio: Arc<Vec<u8>>,
}
struct Delivery {
    id: String,
    preview: String,
    viewer: String,
    deadline: Instant,
    fetched: bool,
    acknowledged: bool,
}
struct ActiveRoute {
    source: Source,
    route: Route,
    language: Language,
    live_mode: Option<LiveMode>,
    interpretation: Option<crate::engine::Interpretation>,
    input: Option<mpsc::Sender<StreamInput>>,
    stop: Option<watch::Sender<bool>>,
    cancel: watch::Sender<bool>,
    renewal_fence: Option<watch::Sender<bool>>,
    ready: bool,
    renewing: bool,
    handover: bool,
    finish_sent: bool,
}
struct SourceState {
    closed: bool,
    interrupted: bool,
    sequence: u32,
    allowance: f64,
    updated: Instant,
}
struct Connection {
    id: String,
    session: String,
    events: mpsc::Sender<Value>,
    heartbeat: Instant,
}
struct Memory {
    bootstrap: Option<Ticket>,
    public_ticket: Option<Ticket>,
    operator: Option<Session>,
    viewer: Option<Session>,
    operator_connection: Option<Connection>,
    public_connection: Option<Connection>,
    routes: BTreeMap<String, ActiveRoute>,
    webrtc_reserved: bool,
    sources: [SourceState; 2],
    frames: u64,
    previews: BTreeMap<String, Clip>,
    delivery: Option<Delivery>,
    finish: Option<FinishState>,
    last_issue: Option<IssueDiagnostic>,
    resolved_provider_failure: Option<(ProviderFailure, IssueDiagnostic)>,
}

pub struct Pilot {
    repeatable_sessions: bool,
    initial_generation: u64,
    origins: auth::LocalOrigins,
    memory: Mutex<Memory>,
    backend: Arc<dyn ProviderBackend>,
    started_at: Instant,
    deadline: Option<Instant>,
    stopped: StopLatch,
    finishing: AtomicBool,
    manual_paused: AtomicBool,
    manual_retired: AtomicBool,
    generation: AtomicU64,
    stop: watch::Sender<bool>,
    public_changed: watch::Sender<u64>,
    finish_changed: watch::Sender<u64>,
    tts: Arc<Semaphore>,
    max_frames: Option<u64>,
}

pub struct Subscription {
    pub id: String,
    pub session: String,
    pub events: mpsc::Receiver<Value>,
}

impl Pilot {
    pub fn new(
        backend: Arc<dyn ProviderBackend>,
        duration: Duration,
        max_audio_seconds: u64,
    ) -> Result<(Arc<Self>, String), &'static str> {
        Self::new_with_origins(
            backend,
            duration,
            max_audio_seconds,
            auth::LocalOrigins::default(),
        )
    }
    pub fn new_with_origins(
        backend: Arc<dyn ProviderBackend>,
        duration: Duration,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
    ) -> Result<(Arc<Self>, String), &'static str> {
        Self::with_lifetime(backend, Some(duration), max_audio_seconds, origins)
    }
    /// Persistent local control has no artificial process deadline. Provider
    /// windows, budgets and each individual operation remain independently bounded.
    pub fn new_persistent(
        backend: Arc<dyn ProviderBackend>,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
    ) -> Result<(Arc<Self>, String), &'static str> {
        Self::with_lifetime(backend, None, max_audio_seconds, origins)
    }
    pub(crate) fn new_hosted(
        backend: Arc<dyn ProviderBackend>,
        duration: Option<Duration>,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
        generation: u64,
        repeatable_sessions: bool,
        process_started: Instant,
    ) -> Result<(Arc<Self>, String), &'static str> {
        Self::with_lifetime_options(
            backend,
            duration,
            max_audio_seconds,
            origins,
            generation,
            repeatable_sessions,
            Some(process_started),
        )
    }
    fn with_lifetime(
        backend: Arc<dyn ProviderBackend>,
        duration: Option<Duration>,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
    ) -> Result<(Arc<Self>, String), &'static str> {
        Self::with_lifetime_options(
            backend,
            duration,
            max_audio_seconds,
            origins,
            1,
            false,
            None,
        )
    }
    fn with_lifetime_options(
        backend: Arc<dyn ProviderBackend>,
        duration: Option<Duration>,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
        generation: u64,
        repeatable_sessions: bool,
        process_started: Option<Instant>,
    ) -> Result<(Arc<Self>, String), &'static str> {
        if !(1..((1_u64 << 53) - 1)).contains(&generation)
            || (repeatable_sessions && duration.is_some())
        {
            return Err("invalid_session_generation");
        }
        if duration
            .is_some_and(|duration| duration.is_zero() || duration > Duration::from_secs(7200))
            || !(1..=14400).contains(&max_audio_seconds)
        {
            return Err("invalid_limits");
        }
        let bootstrap = auth::secret()?;
        let now = Instant::now();
        let deadline = duration.map(|duration| now + duration);
        let (stop, _) = watch::channel(false);
        let (public_changed, _) = watch::channel(0);
        let source = || SourceState {
            closed: false,
            interrupted: false,
            sequence: 0,
            allowance: 10.0,
            updated: now,
        };
        Ok((
            Arc::new(Self {
                repeatable_sessions,
                initial_generation: generation,
                origins,
                memory: Mutex::new(Memory {
                    bootstrap: Some(Ticket::new(
                        &bootstrap,
                        deadline.unwrap_or(now + Duration::from_secs(120)),
                    )),
                    public_ticket: None,
                    operator: None,
                    viewer: None,
                    operator_connection: None,
                    public_connection: None,
                    routes: BTreeMap::new(),
                    webrtc_reserved: false,
                    sources: [source(), source()],
                    frames: 0,
                    previews: BTreeMap::new(),
                    delivery: None,
                    finish: None,
                    last_issue: None,
                    resolved_provider_failure: None,
                }),
                backend,
                started_at: process_started.unwrap_or(now),
                deadline,
                stopped: StopLatch::new(),
                finishing: AtomicBool::new(false),
                manual_paused: AtomicBool::new(false),
                manual_retired: AtomicBool::new(false),
                generation: AtomicU64::new(generation),
                stop,
                public_changed,
                finish_changed: watch::channel(0).0,
                tts: Arc::new(Semaphore::new(1)),
                max_frames: duration.map(|_| max_audio_seconds * 50),
            }),
            bootstrap,
        ))
    }
    /// Exclusive browser-media reservation shares the route admission lock.
    pub(crate) fn claim_webrtc(&self, generation: u64) -> Result<(), ApiError> {
        let mut memory = self.memory.lock().map_err(|_| ApiError::Unavailable)?;
        self.positive_locked(generation)?;
        if !self.repeatable_sessions || !self.is_continuous() || !self.backend.enabled() {
            return Err(ApiError::Forbidden);
        }
        if memory.webrtc_reserved
            || !memory.routes.is_empty()
            || !memory.previews.is_empty()
            || memory.delivery.is_some()
            || memory
                .operator_connection
                .as_ref()
                .is_none_or(|connection| {
                    connection.heartbeat.elapsed() >= self.heartbeat_lease(Role::Operator)
                        || memory
                            .operator
                            .as_ref()
                            .is_none_or(|operator| operator.id != connection.session)
                })
        {
            return Err(ApiError::Conflict);
        }
        memory.webrtc_reserved = true;
        Ok(())
    }
    pub(crate) fn release_webrtc(&self, generation: u64) {
        if self.initial_generation == generation
            && let Ok(mut memory) = self.memory.lock()
        {
            memory.webrtc_reserved = false;
        }
    }
    pub fn generation(&self) -> u64 {
        self.generation.load(Ordering::Acquire)
    }
    pub(crate) fn stopped_generation(&self) -> Option<u64> {
        let generation = self.generation();
        (generation == self.initial_generation + 1).then_some(generation)
    }
    pub fn origins(&self) -> auth::LocalOrigins {
        self.origins
    }
    pub fn is_stopped(&self) -> bool {
        self.stopped.load(Ordering::Acquire) || self.expired()
    }
    pub fn is_finishing(&self) -> bool {
        self.finishing.load(Ordering::Acquire)
    }
    /// Final socket fence for queued generated fragments. ASR observations and
    /// terminal controls remain readable during source Finish, even after the
    /// corresponding route has retired and its final items are already queued.
    pub fn is_manual_paused(&self) -> bool {
        self.manual_paused.load(Ordering::Acquire)
    }
    pub(crate) fn mark_pause_retired(&self, retired: bool) {
        if retired && self.is_manual_paused() {
            self.manual_retired.store(true, Ordering::Release);
        }
    }
    fn paused_control_alive(&self, role: Role) -> bool {
        role == Role::Operator
            && self.is_manual_paused()
            && self.manual_retired.load(Ordering::Acquire)
    }
    pub(crate) fn pause_processing(&self, generation: u64) -> Result<(), ApiError> {
        let mut memory = self.memory.lock().map_err(|_| ApiError::Unavailable)?;
        if !self.repeatable_sessions {
            return Err(ApiError::Forbidden);
        }
        if self.is_stopped() || self.is_finishing() || self.generation() != generation {
            return Err(ApiError::Conflict);
        }
        if self.is_manual_paused() {
            return Ok(());
        }
        // A manual pause cannot launder an earlier fatal/source failure into
        // resumable authority. Those workspaces still require New session.
        if self.backend.failure_cause().is_some() || memory.sources.iter().any(|s| s.closed) {
            return Err(ApiError::Conflict);
        }
        self.manual_paused.store(true, Ordering::Release);
        self.backend.revoke();
        self.finish_changed.send_modify(|epoch| *epoch += 1);
        for route in memory.routes.values() {
            route.cancel.send_replace(true);
            if let Some(fence) = &route.renewal_fence {
                fence.send_replace(true);
            }
            if let Some(stop) = &route.stop {
                stop.send_replace(true);
            }
        }
        memory.routes.clear();
        for source in &mut memory.sources {
            source.interrupted = true;
        }
        memory.previews.clear();
        memory.delivery = None;
        self.public_changed.send_modify(|epoch| *epoch += 1);
        Ok(())
    }
    /// A manual pause suppresses every new content frame. One already-started
    /// bounded operator write may finish framing so Pause does not tear down
    /// the control socket. Its client has already fenced playback. Hard Stop,
    /// auth/connection loss, Finish and the write deadline still dominate.
    pub(crate) fn in_flight_event_current(&self, role: Role, event: &Value, started: bool) -> bool {
        self.event_current(role, event)
            || (started
                && role == Role::Operator
                && self.is_manual_paused()
                && !self.is_stopped()
                && !self.is_finishing()
                && event["generation"].as_u64() == Some(self.generation()))
    }
    pub fn event_current(&self, role: Role, event: &Value) -> bool {
        if self.is_stopped() {
            return false;
        }
        if self.is_manual_paused() {
            return role == Role::Operator
                && matches!(event["type"].as_str(), Some("heartbeat" | "hello"));
        }
        if !self.is_finishing() {
            return true;
        }
        role == Role::Operator
            && match event["type"].as_str() {
                Some("audio" | "preview") => false,
                Some("transcript") => event["provenance"] == "source_asr",
                _ => true,
            }
    }
    pub fn deadline(&self) -> Option<Instant> {
        self.deadline
    }
    pub fn is_continuous(&self) -> bool {
        self.deadline.is_none()
    }
    fn expired(&self) -> bool {
        self.deadline
            .is_some_and(|deadline| Instant::now() >= deadline)
    }
    pub fn operation_deadline(&self, limit: Duration) -> Instant {
        let deadline = Instant::now() + limit;
        self.deadline
            .map_or(deadline, |process| deadline.min(process))
    }
    pub fn stop_receiver(&self) -> watch::Receiver<bool> {
        self.stop.subscribe()
    }
    pub fn public_receiver(&self) -> watch::Receiver<u64> {
        self.public_changed.subscribe()
    }
    pub fn enabled(&self) -> bool {
        self.backend.enabled()
    }
    pub fn stop(&self, reason: &'static str) {
        let code = StopCode::from_raw(reason);
        if self.stopped.try_stop(code, self.elapsed_seconds()) {
            self.complete_stop(code);
        }
    }

    /// All concurrent POSTs observe the winning Stop fence. HTTP cancellation
    /// cannot interrupt it, and a second caller never acknowledges the old generation.
    pub(crate) async fn operator_stop(self: &Arc<Self>) -> Result<u64, ApiError> {
        let mut published = self.stop_receiver();
        let code = StopCode::OperatorStop;
        if self.stopped.try_stop(code, self.elapsed_seconds()) {
            let pilot = Arc::clone(self);
            tokio::task::spawn_blocking(move || pilot.complete_stop(code));
        }
        tokio::time::timeout(
            Duration::from_secs(2),
            published.wait_for(|stopped| *stopped),
        )
        .await
        .map_err(|_| ApiError::Unavailable)?
        .map_err(|_| ApiError::Unavailable)?;
        self.stopped_generation().ok_or(ApiError::Unavailable)
    }

    fn complete_stop(&self, code: StopCode) {
        // Fence opens and synthesis before publishing the terminal
        // generation. A concurrent receipt cannot miss a later reservation.
        self.backend.revoke();
        let generation = self.generation.fetch_add(1, Ordering::AcqRel) + 1;
        self.stop.send_replace(true);
        let mut memory = self.memory.lock().expect("pilot state");
        for route in memory.routes.values() {
            route.cancel.send_replace(true);
            if let Some(stop) = &route.stop {
                stop.send_replace(true);
            }
        }
        memory.routes.clear();
        memory.previews.clear();
        memory.delivery = None;
        let event = json!({"type":"stopped","generation":generation,"reason":code});
        if let Some(c) = &memory.operator_connection {
            let _ = c.events.try_send(event.clone());
        }
        if let Some(c) = &memory.public_connection {
            let _ = c.events.try_send(event);
        }
    }
    pub fn positive(&self, generation: u64) -> Result<(), ApiError> {
        self.observe(generation)?;
        if self
            .memory
            .lock()
            .map_err(|_| ApiError::Unavailable)?
            .webrtc_reserved
        {
            return Err(ApiError::Conflict);
        }
        self.positive_locked(generation)
    }
    // Use under `memory` without calling Stop, which acquires that same lock.
    fn positive_locked(&self, generation: u64) -> Result<(), ApiError> {
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if self.generation() != generation {
            return Err(ApiError::Conflict);
        }
        Ok(())
    }
    fn observe(&self, generation: u64) -> Result<(), ApiError> {
        if self.is_stopped() {
            self.stop("deadline");
            return Err(ApiError::Stopped);
        }
        if generation != self.generation() {
            return Err(ApiError::Conflict);
        }
        Ok(())
    }
    pub fn authorize(
        &self,
        headers: &HeaderMap,
        role: Role,
        mutation: bool,
    ) -> Result<String, ApiError> {
        self.authorize_credential(auth::bearer(headers), role, mutation.then_some(headers))
    }
    pub(crate) fn authorize_events(
        &self,
        headers: &HeaderMap,
        role: Role,
    ) -> Result<String, ApiError> {
        self.authorize_credential(auth::event_bearer(headers), role, None)
    }
    fn authorize_credential(
        &self,
        credential: Option<&str>,
        role: Role,
        mutation: Option<&HeaderMap>,
    ) -> Result<String, ApiError> {
        if self.expired() {
            self.stop("deadline");
            return Err(ApiError::Unauthorized);
        }
        let memory = self.memory.lock().expect("pilot state");
        let session = match role {
            Role::Operator => &memory.operator,
            Role::Public => &memory.viewer,
        }
        .as_ref()
        .ok_or(ApiError::Unauthorized)?;
        if !session.matches(credential) {
            return Err(ApiError::Unauthorized);
        }
        if mutation.is_some_and(|headers| !session.csrf_matches(headers)) {
            return Err(ApiError::Forbidden);
        }
        Ok(session.id.clone())
    }
    pub(crate) fn rollover_authority_current(&self, session: &str) -> bool {
        self.memory.lock().is_ok_and(|memory| {
            memory
                .operator
                .as_ref()
                .is_some_and(|operator| operator.id == session)
        })
    }
    pub(crate) fn rollover_csrf(&self, headers: &HeaderMap) -> bool {
        self.memory.lock().is_ok_and(|memory| {
            memory
                .operator
                .as_ref()
                .is_some_and(|operator| operator.csrf_matches(headers))
        })
    }
    pub fn session(&self, headers: &HeaderMap, role: Role) -> Result<Value, ApiError> {
        self.authorize(headers, role, false)?;
        let memory = self.memory.lock().expect("pilot state");
        let session = match role {
            Role::Operator => &memory.operator,
            Role::Public => &memory.viewer,
        }
        .as_ref()
        .ok_or(ApiError::Unauthorized)?;
        Ok(self.session_value(session))
    }
    fn session_value(&self, session: &Session) -> Value {
        let active = self.enabled()
            && !self.is_stopped()
            && !self.finishing.load(Ordering::Acquire)
            && !self.is_manual_paused();
        let private = active && session.role == Role::Operator;
        let connection_limit = if self.is_continuous() && session.role == Role::Operator {
            "Memory-only data. Active speech connections recover after network interruptions; lost browser control requires a fresh session. Missing audio is never replayed."
        } else {
            "Memory-only data; no automatic reconnect or playback replay."
        };
        let mut view = json!({"role":session.role,"csrf":session.csrf,"generation":self.generation(),"stopped":self.is_stopped(),"providers_enabled":self.enabled(),"continuous":self.is_continuous(),"repeatable_sessions":self.repeatable_sessions && session.role == Role::Operator,"expires_in_seconds":self.deadline.map(|deadline| deadline.saturating_duration_since(Instant::now()).as_secs()),"capabilities":{"source_captions":private&&self.backend.supports(Route::Transcription),"private_translation":private&&self.backend.supports(Route::Translation),"live_assistant":private&&self.backend.supports_live_mode(LiveMode::Assistant),"live_interpreter":private&&self.backend.supports_live_mode(LiveMode::Interpretation),"prepared_public_speech":active&&self.backend.supports_tts()},"limitations":["Supervised local pilot; live language and call routing quality are unverified.","No proof of participant identity, consent, microphone mute, or remote hearing.",connection_limit]});
        if session.role == Role::Operator
            && self.backend.vendor() == crate::budget::ProviderVendor::Gemini
        {
            view["provider_vendor"] = json!("gemini");
        }
        if self.repeatable_sessions && session.role == Role::Operator {
            view["session_generation"] = json!(self.initial_generation);
        }
        view
    }
    pub fn redeem(&self, value: &str, role: Role) -> Result<(Value, String), ApiError> {
        self.positive(self.generation())?;
        let mut memory = self.memory.lock().expect("pilot state");
        self.positive_locked(self.generation())?;
        let ticket = match role {
            Role::Operator => &mut memory.bootstrap,
            Role::Public => &mut memory.public_ticket,
        };
        if !ticket.as_ref().is_some_and(|t| t.matches(value)) {
            return Err(ApiError::Unauthorized);
        }
        *ticket = None;
        let (session, token) = Session::new(role).map_err(|_| ApiError::Unavailable)?;
        let view = self.session_value(&session);
        match role {
            Role::Operator => memory.operator = Some(session),
            Role::Public => memory.viewer = Some(session),
        }
        Ok((view, token))
    }
    pub fn public_ticket(&self, generation: u64) -> Result<Value, ApiError> {
        self.positive(generation)?;
        let value = auth::secret().map_err(|_| ApiError::Unavailable)?;
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        memory.public_ticket = Some(Ticket::new(
            &value,
            self.operation_deadline(Duration::from_secs(120)),
        ));
        memory.viewer = None;
        memory.public_connection = None;
        Self::unknown_delivery(&mut memory);
        self.public_changed.send_modify(|epoch| *epoch += 1);
        Ok(
            json!({"ticket":value,"expires_in_seconds":120,"path":"/public","origin":self.origins.origin(Role::Public)}),
        )
    }
    pub fn logout(&self, role: Role) {
        if role == Role::Operator {
            self.stop("operator_stop");
        }
        let mut memory = self.memory.lock().expect("pilot state");
        match role {
            Role::Operator => memory.operator = None,
            Role::Public => {
                memory.viewer = None;
                memory.public_connection = None;
                Self::unknown_delivery(&mut memory);
                self.public_changed.send_modify(|e| *e += 1);
            }
        }
    }
    pub fn subscribe(&self, session: String, role: Role) -> Result<Subscription, ApiError> {
        self.positive(self.generation())?;
        let mut memory = self.memory.lock().expect("pilot state");
        self.positive_locked(self.generation())?;
        let active = match role {
            Role::Operator => &mut memory.operator_connection,
            Role::Public => &mut memory.public_connection,
        };
        if active.is_some() {
            return Err(ApiError::Conflict);
        }
        let (events, receiver) = mpsc::channel(32);
        let id = auth::secret().map_err(|_| ApiError::Unavailable)?;
        *active = Some(Connection {
            id: id.clone(),
            session: session.clone(),
            events,
            heartbeat: Instant::now(),
        });
        Ok(Subscription {
            id,
            session,
            events: receiver,
        })
    }
    fn heartbeat_lease(&self, role: Role) -> Duration {
        Duration::from_secs(if self.is_continuous() && role == Role::Operator {
            30
        } else {
            5
        })
    }
    pub fn connection_current(&self, id: &str, session_id: &str, role: Role) -> bool {
        if self.is_stopped() {
            return false;
        }
        let memory = self.memory.lock().expect("pilot state");
        let (session, connection) = match role {
            Role::Operator => (&memory.operator, &memory.operator_connection),
            Role::Public => (&memory.viewer, &memory.public_connection),
        };
        session.as_ref().is_some_and(|s| s.id == session_id)
            && connection.as_ref().is_some_and(|c| {
                c.id == id
                    && c.session == session_id
                    && (self.paused_control_alive(role)
                        || c.heartbeat.elapsed() < self.heartbeat_lease(role))
            })
    }
    pub fn heartbeat(&self, id: &str, role: Role, generation: u64) -> Result<(), ApiError> {
        self.observe(generation)?;
        let mut memory = self.memory.lock().expect("pilot state");
        let connection = match role {
            Role::Operator => &mut memory.operator_connection,
            Role::Public => &mut memory.public_connection,
        }
        .as_mut()
        .ok_or(ApiError::Unauthorized)?;
        if connection.id != id
            || (!self.paused_control_alive(role)
                && connection.heartbeat.elapsed() >= self.heartbeat_lease(role))
        {
            return Err(ApiError::Unauthorized);
        }
        connection.heartbeat = Instant::now();
        Ok(())
    }
    pub fn disconnected(&self, id: &str, role: Role) {
        let mut memory = self.memory.lock().expect("pilot state");
        let connection = match role {
            Role::Operator => &mut memory.operator_connection,
            Role::Public => &mut memory.public_connection,
        };
        if connection.as_ref().is_none_or(|c| c.id != id) {
            return;
        }
        *connection = None;
        if role == Role::Public {
            Self::unknown_delivery(&mut memory);
            self.public_changed.send_modify(|e| *e += 1);
        }
        drop(memory);
        if role == Role::Operator {
            self.stop("operator_disconnected");
        }
    }
    fn emit_private(&self, event: Value) {
        if self.is_stopped() || self.is_manual_paused() {
            return;
        }
        let failed = self
            .memory
            .lock()
            .expect("pilot state")
            .operator_connection
            .as_ref()
            .is_none_or(|c| c.events.try_send(event).is_err());
        if failed {
            self.stop("backpressure");
        }
    }
    pub fn source_gap(&self, source: Source, code: &'static str) {
        self.fail_source(None, source, code, None);
    }
    fn route_gap(&self, id: &str, source: Source, code: &'static str) {
        self.fail_source(Some(id), source, code, None);
    }
    fn route_failure(&self, id: &str, source: Source, error: ProviderError) {
        self.fail_source(Some(id), source, error.code(), Some(error));
    }
    fn fail_source(
        &self,
        route_id: Option<&str>,
        source: Source,
        code: &'static str,
        error: Option<ProviderError>,
    ) {
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.is_manual_paused() {
            return;
        }
        // A generated route may have been retired by Finish after its event
        // was dequeued. That stale callback cannot cancel surviving ASR work.
        if route_id.is_some_and(|id| !memory.routes.contains_key(id)) {
            return;
        }
        let route = route_id.and_then(|id| memory.routes.get(id).map(|active| active.route));
        let known_failure = self.backend.failure_cause();
        let cause = known_failure;
        let retained_cause = known_failure.and_then(|cause| {
            memory
                .resolved_provider_failure
                .filter(|(resolved, _)| *resolved == cause)
                .map(|(_, issue)| issue)
        });
        // The backend knows the original route kind, not its source. Resolve
        // that source only from an admitted still-owned route or its previously
        // recorded issue; never attribute the sibling's source to the cause.
        // Duplicate kinds may be pending on different sources before backend
        // admission rejects one. A pending or later sole surviving binding
        // cannot establish the source; the installed handle must identify it.
        let causal_source = cause.and_then(|cause| {
            let mut matching = memory.routes.values().filter(|active| {
                active.route == cause.route && active.input.is_some() && active.stop.is_some()
            });
            let first = matching.next()?;
            matching.next().is_none().then_some(first.source)
        });
        let code = if let Some(cause) =
            cause.filter(|_| causal_source.is_some() || retained_cause.is_some())
        {
            if retained_cause.is_none()
                && self.record_provider_issue(
                    &mut memory,
                    causal_source.expect("known causal route"),
                    Some(cause.route),
                    cause.error,
                )
            {
                memory.resolved_provider_failure = memory.last_issue.map(|issue| (cause, issue));
            }
            cause.error.code()
        } else {
            if !memory.sources[source.index()].closed && retained_cause.is_none() {
                if let Some(error) = error {
                    self.record_provider_issue(&mut memory, source, route, error);
                } else {
                    self.record_issue(&mut memory, source, route, code);
                }
            }
            code
        };
        memory.sources[source.index()].closed = true;
        let removed: Vec<_> = memory
            .routes
            .iter()
            .filter(|(_, r)| r.source == source)
            .map(|(id, _)| id.clone())
            .collect();
        for id in &removed {
            let route = memory.routes.remove(id).expect("existing source route");
            route.cancel.send_replace(true);
            if let Some(stop) = route.stop {
                stop.send_replace(true);
            }
        }
        let failed = !Self::queue_private(
            &memory,
            json!({"type":"gap","generation":self.initial_generation,"source":source,"code":code}),
        );
        if let Some(finish) = &mut memory.finish {
            finish.incomplete = true;
        }
        for id in removed {
            self.retire_finish_route(&mut memory, &id, false);
        }
        drop(memory);
        if failed {
            self.stop("backpressure");
        }
    }
    pub fn close_route(&self, route_id: &str) -> Result<(), ApiError> {
        self.retire_route(route_id, false)
    }
    pub fn start_route(
        self: &Arc<Self>,
        generation: u64,
        source: Source,
        route: Route,
        language: Language,
    ) -> Result<String, ApiError> {
        self.start_route_with_mode(generation, source, route, language, None)
    }
    pub fn start_route_with_mode(
        self: &Arc<Self>,
        generation: u64,
        source: Source,
        route: Route,
        language: Language,
        mode: Option<LiveMode>,
    ) -> Result<String, ApiError> {
        self.start_route_with_options(
            generation,
            source,
            route,
            language,
            crate::engine::RouteOptions {
                live_mode: mode,
                transcription: None,
                interpretation: None,
            },
        )
    }
    pub fn start_route_with_options(
        self: &Arc<Self>,
        generation: u64,
        source: Source,
        route: Route,
        language: Language,
        options: crate::engine::RouteOptions,
    ) -> Result<String, ApiError> {
        self.positive(generation)?;
        if let Some(policy) = options.interpretation {
            policy.validate(language).map_err(|_| ApiError::Invalid)?;
            if !self.repeatable_sessions
                || route != Route::Live
                || options.live_mode != Some(LiveMode::Interpretation)
            {
                return Err(ApiError::Forbidden);
            }
        }
        // An expanded source anchor is explicit session configuration, not a
        // legacy target-language fallback. Require the saved repeatable scope
        // and literal ASR hints that actually include that anchor.
        let expanded_source = self.repeatable_sessions
            && self.is_continuous()
            && route == Route::Transcription
            && options
                .transcription
                .as_ref()
                .is_some_and(|hints| hints.languages.contains(&language));
        if !language.legacy() && options.interpretation.is_none() && !expanded_source {
            return Err(ApiError::Invalid);
        }
        if (!self.repeatable_sessions || !self.is_continuous())
            && options
                .transcription
                .as_ref()
                .is_some_and(|h| h.languages.iter().any(|l| !l.legacy()))
        {
            return Err(ApiError::Forbidden);
        }
        if let Some(hints) = &options.transcription {
            if route != Route::Transcription {
                return Err(ApiError::Invalid);
            }
            hints.validate().map_err(|_| ApiError::Invalid)?;
        }
        let live_mode = route
            .resolve_live_mode(options.live_mode)
            .map_err(|_| ApiError::Invalid)?;
        if !self.backend.supports(route)
            || live_mode.is_some_and(|mode| !self.backend.supports_live_mode(mode))
        {
            return Err(ApiError::Unavailable);
        }
        let id = auth::secret().map_err(|_| ApiError::Unavailable)?;
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if memory.operator_connection.is_none() || memory.sources[source.index()].closed {
            return Err(ApiError::Stopped);
        }
        if memory.webrtc_reserved {
            return Err(ApiError::Conflict);
        }
        if memory.routes.len() >= 6 {
            return Err(ApiError::Capacity);
        }
        if memory
            .routes
            .values()
            .any(|r| r.source == source && r.route == route)
        {
            return Err(ApiError::Conflict);
        }
        memory.routes.insert(
            id.clone(),
            ActiveRoute {
                source,
                route,
                language,
                live_mode,
                interpretation: options.interpretation,
                input: None,
                stop: None,
                cancel: watch::channel(false).0,
                renewal_fence: None,
                ready: false,
                renewing: false,
                handover: false,
                finish_sent: false,
            },
        );
        drop(memory);
        let mut event = json!({"type":"route","generation":generation,"route_id":id,"source":source,"route":route,"language":language,"live_mode":live_mode,"state":"starting","code":"awaiting_provider","expires_in_seconds":null});
        if let Some(policy) = options.interpretation {
            event["interpretation"] = json!(policy);
        }
        self.emit_private(event);
        let pilot = Arc::clone(self);
        let task_id = id.clone();
        tokio::spawn(async move {
            pilot
                .run_route(task_id, generation, source, route, language, options)
                .await;
        });
        Ok(id)
    }
    async fn run_route(
        self: Arc<Self>,
        id: String,
        generation: u64,
        source: Source,
        route: Route,
        language: Language,
        options: crate::engine::RouteOptions,
    ) {
        if self.positive(generation).is_err() {
            return;
        }
        let mut cancelled = {
            let memory = self.memory.lock().expect("pilot state");
            let Some(active) = memory.routes.get(&id) else {
                return;
            };
            active.cancel.subscribe()
        };
        if *cancelled.borrow() {
            return;
        }
        let result = tokio::select! {biased;_=cancelled.changed()=>return,result=self.provider_wait(
            generation,
            Duration::from_secs(10),
            match route {
                Route::Transcription => self.backend.open_transcription(language, options.transcription.unwrap_or_default()),
                Route::Live if options.interpretation.is_some() => self.backend.open_interpretation(language, options.interpretation.expect("checked")),
                _ => self.backend.open_with_mode(route, language, options.live_mode),
            },
        )=>result};
        // Preserve only fixed local categories. The outer wait can end on its
        // deadline or revoked authority; the latter must not emit a new failure.
        let opened = result
            .map_err(|_| ProviderError::Deadline)
            .and_then(|opened| opened);
        let handle = match opened {
            Ok(handle) => handle,
            Err(error) => {
                if self.positive(generation).is_ok() {
                    self.route_failure(&id, source, error);
                }
                return;
            }
        };
        self.consume_route(id, generation, source, route, language, handle)
            .await;
    }
    pub(crate) async fn provider_wait<T>(
        &self,
        generation: u64,
        limit: Duration,
        future: impl Future<Output = T>,
    ) -> Result<T, ApiError> {
        let mut stop = self.stop_receiver();
        let mut finishing = self.finish_changed.subscribe();
        let deadline = self.operation_deadline(limit);
        tokio::pin!(future);
        let checked = std::future::poll_fn(|cx| {
            if self.is_stopped()
                || self.finishing.load(Ordering::Acquire)
                || self.is_manual_paused()
                || self.generation() != generation
                || Instant::now() >= deadline
            {
                Poll::Ready(Err(ApiError::Stopped))
            } else {
                future.as_mut().poll(cx).map(Ok)
            }
        });
        tokio::select! {biased;_=stop.changed()=>Err(ApiError::Stopped),_=finishing.changed()=>Err(ApiError::Stopped),_=tokio::time::sleep_until(deadline.into())=>Err(ApiError::Unavailable),result=checked=>result}
    }
    async fn consume_route(
        self: &Arc<Self>,
        id: String,
        generation: u64,
        source: Source,
        route: Route,
        language: Language,
        mut handle: StreamHandle,
    ) {
        let live_mode = {
            let mut memory = self.memory.lock().expect("pilot state");
            if self.is_stopped() || self.generation() != generation {
                return;
            }
            let Some(active) = memory.routes.get_mut(&id) else {
                return;
            };
            active.input = Some(handle.input.clone());
            active.stop = Some(handle.stop.clone());
            active.renewal_fence = handle.renewal_fence.clone();
            active.live_mode
        };
        let mut stop = self.stop_receiver();
        let mut route_stop = handle.stop.subscribe();
        let mut drained = false;
        let mut window = 1_u64;
        let mut transcript_items = std::collections::BTreeMap::<String, (u32, u32, bool)>::new();
        let mut transcript_sequences = std::collections::BTreeMap::<u32, String>::new();
        let mut source_drafts = source_drafts::SourceDrafts::default();
        loop {
            if self.is_stopped()
                || *route_stop.borrow()
                || !self
                    .memory
                    .lock()
                    .expect("pilot state")
                    .routes
                    .contains_key(&id)
            {
                break;
            }
            let event = tokio::select! { biased; _ = stop.changed() => break, _ = route_stop.changed() => break, event = handle.events.recv() => event };
            if self.observe(generation).is_err()
                || !self
                    .memory
                    .lock()
                    .expect("pilot state")
                    .routes
                    .contains_key(&id)
            {
                break;
            }
            let Some(event) = event else {
                self.route_gap(&id, source, "provider_closed");
                break;
            };
            if drained && !matches!(event, StreamEvent::Closed { .. } | StreamEvent::Error(_)) {
                self.route_gap(&id, source, "provider_protocol");
                break;
            }
            if self.is_continuous()
                && matches!(
                    event,
                    StreamEvent::Transcript { .. }
                        | StreamEvent::Audio { .. }
                        | StreamEvent::SourceDraft { .. }
                        | StreamEvent::LiveTranscript { .. }
                )
                && self
                    .memory
                    .lock()
                    .expect("pilot state")
                    .routes
                    .get(&id)
                    .is_none_or(|r| !r.ready)
            {
                self.route_gap(&id, source, "provider_protocol");
                break;
            }
            match event {
                StreamEvent::Handover => {
                    if !self.is_continuous() || drained {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    {
                        let mut memory = self.memory.lock().expect("pilot state");
                        let selected_finish =
                            route == Route::Transcription && Self::finish_selected(&memory, &id);
                        let Some(active) = memory.routes.get_mut(&id) else {
                            break;
                        };
                        if !active.ready
                            || active.renewing
                            || active.handover
                            || ((self.is_finishing() || active.finish_sent) && !selected_finish)
                        {
                            drop(memory);
                            self.route_gap(&id, source, "provider_protocol");
                            break;
                        }
                        // Continue receiving the old source window's finals.
                        // The supervisor owns the input boundary and closure.
                        // A notice queued before Finish may arrive after its
                        // synchronous fence and ordered source barrier. Adopt
                        // it only for that selected source; this admits no new
                        // provider window and preserves final observation.
                        active.handover = true;
                    }
                    self.emit_route_event(&id, json!({"type":"window","generation":generation,"route_id":id,"source":source,"route":route,"window":window,"state":"draining"}));
                }
                StreamEvent::InputSealed => {
                    // This receipt is private to the continuous supervisor.
                    // It neither proves source finals nor belongs on the UI wire.
                    self.route_gap(&id, source, "provider_protocol");
                    break;
                }
                StreamEvent::Recovering {
                    window: next,
                    error,
                    retry_in_seconds,
                } => {
                    if !self.is_continuous()
                        || drained
                        || !error.recoverable()
                        || ![1, 2, 4, 30].contains(&retry_in_seconds)
                        || window.checked_add(1) != Some(next)
                        || next > (1_u64 << 53) - 1
                    {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    {
                        let mut memory = self.memory.lock().expect("pilot state");
                        if self.is_stopped() {
                            break;
                        }
                        if self.is_finishing() {
                            drop(memory);
                            self.route_gap(&id, source, "source_incomplete");
                            break;
                        }
                        let Some(active) = memory.routes.get_mut(&id) else {
                            break;
                        };
                        if active.finish_sent {
                            break;
                        }
                        active.ready = false;
                        active.renewing = true;
                        active.handover = false;
                        // This marks coverage incomplete without closing capture
                        // or inventing a final for any interrupted source item.
                        if route == Route::Transcription {
                            memory.sources[source.index()].interrupted = true;
                        }
                    }
                    transcript_items.clear();
                    transcript_sequences.clear();
                    source_drafts.clear();
                    window = next;
                    self.emit_route_event(&id, json!({"type":"window","generation":generation,"route_id":id,"source":source,"route":route,"window":window,"state":"recovering","code":error.code(),"retry_in_seconds":retry_in_seconds}));
                }
                StreamEvent::Renewing { window: next } => {
                    if !self.is_continuous()
                        || drained
                        || window.checked_add(1) != Some(next)
                        || next > (1_u64 << 53) - 1
                        || transcript_items.values().any(|(_, _, sealed)| !sealed)
                        || !source_drafts.complete(&transcript_items)
                    {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    {
                        let mut memory = self.memory.lock().expect("pilot state");
                        let selected_finish =
                            route == Route::Transcription && Self::finish_selected(&memory, &id);
                        let Some(active) = memory.routes.get_mut(&id) else {
                            break;
                        };
                        if !active.ready
                            || active.renewing
                            || !active.handover
                            || (active.finish_sent && !selected_finish)
                        {
                            drop(memory);
                            self.route_gap(&id, source, "provider_protocol");
                            break;
                        }
                        if self.is_finishing() {
                            // The old window was complete, but Finish raced
                            // with its retirement. The next window has not
                            // been adopted here, and transition input may have
                            // been discarded. Never manufacture completeness
                            // or treat a valid queued marker as malformed.
                            drop(memory);
                            self.route_gap(&id, source, "source_incomplete");
                            break;
                        }
                        active.ready = false;
                        active.renewing = true;
                    }
                    // A supervisor can advance only after confirming the old
                    // source window's final observations and resource closure.
                    // The outer event channel orders these before this marker.
                    transcript_items.clear();
                    transcript_sequences.clear();
                    source_drafts.clear();
                    window = next;
                    self.emit_route_event(&id, json!({"type":"window","generation":generation,"route_id":id,"source":source,"route":route,"window":window,"state":"renewing"}));
                }
                StreamEvent::Ready { expires_in_seconds } => {
                    if !(1..=7200).contains(&expires_in_seconds) {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    {
                        let mut memory = self.memory.lock().expect("pilot state");
                        let Some(active) = memory.routes.get_mut(&id) else {
                            break;
                        };
                        if active.ready || self.is_stopped() {
                            drop(memory);
                            self.route_gap(&id, source, "provider_protocol");
                            break;
                        }
                        active.ready = true;
                        active.renewing = false;
                        active.handover = false;
                    }
                    self.emit_route_event(&id, json!({"type":"route","generation":generation,"route_id":id,"source":source,"route":route,"language":language,"live_mode":live_mode,"state":"ready","code":null,"expires_in_seconds":expires_in_seconds}));
                    if self.is_continuous() {
                        self.emit_route_event(&id, json!({"type":"window","generation":generation,"route_id":id,"source":source,"route":route,"window":window,"state":"ready"}));
                    }
                }
                StreamEvent::SourceDraft {
                    item_id,
                    revision,
                    text,
                    retired,
                } => {
                    let ready = self
                        .memory
                        .lock()
                        .expect("pilot state")
                        .routes
                        .get(&id)
                        .is_some_and(|r| r.ready);
                    if route != Route::Transcription
                        || !ready
                        || !source_drafts.apply(
                            &item_id,
                            revision,
                            &text,
                            retired,
                            transcript_items.contains_key(&item_id),
                        )
                    {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    let mut event = json!({"type":"source_draft","generation":generation,"route_id":id,"source":source,"route":route,"item_id":item_id,"revision":revision,"text":text,"retired":retired});
                    if self.is_continuous() {
                        event["window"] = json!(window);
                    }
                    self.emit_route_event(&id, event);
                }
                StreamEvent::LiveTranscript {
                    provenance,
                    text,
                    start_ms,
                    end_ms,
                } => {
                    let ready = self
                        .memory
                        .lock()
                        .expect("pilot state")
                        .routes
                        .get(&id)
                        .is_some_and(|r| r.ready);
                    if !ready
                        || route != Route::Live
                        || !matches!(
                            provenance,
                            crate::engine::Provenance::LiveInput
                                | crate::engine::Provenance::LiveOutput
                        )
                        || text.is_empty()
                        || text.len() > 16384
                        || text.chars().count() > 4096
                        || text.contains('\0')
                        || !start_ms.is_finite()
                        || !end_ms.is_finite()
                        || start_ms < 0.0
                        || end_ms < start_ms
                        || end_ms > 3_610_000.0
                    {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    let mut event = json!({"type":"live_transcript","generation":generation,"route_id":id,"source":source,"route":route,"provenance":provenance,"text":text,"start_ms":start_ms,"end_ms":end_ms});
                    if self.is_continuous() {
                        event["window"] = json!(window);
                    }
                    self.emit_route_event(&id, event);
                }
                StreamEvent::Transcript {
                    provenance,
                    text,
                    final_fragment,
                    item,
                } => {
                    let valid = matches!(
                        (route, provenance),
                        (Route::Transcription, crate::engine::Provenance::SourceAsr)
                            | (Route::Translation, crate::engine::Provenance::Translation)
                            | (
                                Route::Live,
                                crate::engine::Provenance::LiveInput
                                    | crate::engine::Provenance::LiveOutput
                            )
                    );
                    let source_asr = provenance == crate::engine::Provenance::SourceAsr;
                    let item_valid = if source_asr {
                        item.as_ref().is_some_and(|item| {
                            if !source_drafts.can_bind(&item.id)
                                || item.id.is_empty()
                                || item.id.len() > 128
                                || !item
                                    .id
                                    .bytes()
                                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
                                || !(1..=4096).contains(&item.sequence)
                                || !(1..=4096).contains(&item.revision)
                            {
                                return false;
                            }
                            if let Some((sequence, revision, sealed)) =
                                transcript_items.get(&item.id)
                            {
                                if *sealed
                                    || *sequence != item.sequence
                                    || item.revision != revision + 1
                                {
                                    return false;
                                }
                            } else if transcript_items.len() >= 4096
                                || item.revision != 1
                                || transcript_sequences.contains_key(&item.sequence)
                            {
                                return false;
                            }
                            transcript_sequences.insert(item.sequence, item.id.clone());
                            transcript_items.insert(
                                item.id.clone(),
                                (item.sequence, item.revision, final_fragment),
                            );
                            true
                        })
                    } else {
                        item.is_none()
                    };
                    if !valid
                        || !item_valid
                        || (text.is_empty() && !(source_asr && final_fragment))
                        || text.len() > 16384
                        || text.chars().count() > 4096
                        || text.contains('\0')
                    {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    let mut event = json!({"type":"transcript","generation":generation,"route_id":id,"source":source,"route":route,"provenance":provenance,"text":text,"final":final_fragment,"item":item});
                    if self.is_continuous() {
                        event["window"] = json!(window);
                    }
                    self.emit_route_event(&id, event);
                }
                StreamEvent::Audio { pcm16, sample_rate } => {
                    if route == Route::Transcription
                        || pcm16.is_empty()
                        || pcm16.len() > 48000
                        || pcm16.len() % 2 != 0
                        || ![16000, 24000].contains(&sample_rate)
                    {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    let mut event = json!({"type":"audio","generation":generation,"route_id":id,"source":source,"route":route,"pcm16_base64":STANDARD.encode(pcm16),"sample_rate":sample_rate});
                    if self.is_continuous() {
                        event["window"] = json!(window);
                    }
                    self.emit_route_event(&id, event);
                }
                StreamEvent::Error(error) => {
                    self.route_failure(&id, source, error);
                    break;
                }
                StreamEvent::Drained { completion } => {
                    let memory = self.memory.lock().expect("pilot state");
                    let valid = route == Route::Transcription
                        && completion == crate::engine::DrainCompletion::CommittedTranscriptsFinal
                        && Self::finish_selected(&memory, &id)
                        && memory.routes.get(&id).is_some_and(|r| r.finish_sent)
                        && source_drafts.complete(&transcript_items)
                        && transcript_items.values().all(|(_, _, sealed)| *sealed);
                    drop(memory);
                    if !valid {
                        self.route_gap(&id, source, "provider_protocol");
                        break;
                    }
                    drained = true;
                }
                StreamEvent::Closed { confirmed } => {
                    if confirmed {
                        let _ = self.retire_route(&id, drained);
                    } else {
                        self.route_gap(&id, source, "provider_closed");
                    }
                    break;
                }
            }
        }
    }
    pub fn frame(&self, data: &[u8]) -> Result<(), ApiError> {
        self.positive(self.generation())?;
        if data.len() != 646 || data[0] != 1 || data[1] > 1 {
            self.stop("invalid_frame");
            return Err(ApiError::Invalid);
        }
        let source = if data[1] == 0 {
            Source::RemoteMix
        } else {
            Source::SelfSource
        };
        let sequence = u32::from_le_bytes(data[2..6].try_into().map_err(|_| ApiError::Invalid)?);
        let mut memory = self.memory.lock().expect("pilot state");
        self.positive_locked(self.generation())?;
        let frame_budget = self.max_frames.is_some_and(|limit| memory.frames >= limit);
        let next_frames = memory.frames.checked_add(1);
        let stream = &mut memory.sources[source.index()];
        let now = Instant::now();
        stream.allowance =
            (stream.allowance + now.duration_since(stream.updated).as_secs_f64() * 50.0).min(10.0);
        stream.updated = now;
        let next_sequence = stream.sequence.checked_add(1);
        let error = if stream.closed {
            Some("source_gap")
        } else if sequence != stream.sequence {
            Some("sequence_gap")
        } else if frame_budget || next_frames.is_none() || next_sequence.is_none() {
            Some("input_audio_limit")
        } else if stream.allowance < 1.0 {
            Some("backpressure")
        } else {
            None
        };
        if let Some(code) = error {
            drop(memory);
            self.source_gap(source, code);
            return Err(ApiError::Stopped);
        }
        stream.sequence = next_sequence.expect("checked before admitting a frame");
        stream.allowance -= 1.0;
        memory.frames = next_frames.expect("checked before admitting a frame");
        let mut failed = false;
        for active in memory
            .routes
            .values()
            .filter(|r| r.source == source && (r.ready || r.renewing))
        {
            if let Some(input) = &active.input
                && input
                    .try_send(StreamInput::Pcm16(data[6..].to_vec()))
                    .is_err()
            {
                failed = true;
            }
        }
        drop(memory);
        if failed {
            self.source_gap(source, "backpressure");
            return Err(ApiError::Capacity);
        }
        Ok(())
    }
    pub fn live_text(&self, generation: u64, id: &str, text: String) -> Result<(), ApiError> {
        self.positive(generation)?;
        bounded_text(&text)?;
        if text.len() > 400 {
            return Err(ApiError::Invalid);
        }
        let memory = self.memory.lock().expect("pilot state");
        self.positive_locked(generation)?;
        let route = memory.routes.get(id).ok_or(ApiError::Invalid)?;
        if route.route != Route::Live
            || route.live_mode != Some(LiveMode::Assistant)
            || !route.ready
        {
            return Err(ApiError::Invalid);
        }
        route
            .input
            .as_ref()
            .ok_or(ApiError::Unavailable)?
            .try_send(StreamInput::Text(text))
            .map_err(|_| ApiError::Capacity)
    }
    pub async fn prepare(
        self: &Arc<Self>,
        generation: u64,
        text: String,
        language: Language,
    ) -> Result<Preview, ApiError> {
        self.positive(generation)?;
        // Expanded languages belong only to the versioned Live interpretation
        // extension, never to historical prepared public-speech allowances.
        if !language.legacy() {
            return Err(ApiError::Invalid);
        }
        bounded_text(&text)?;
        if text.len() > 2000 {
            return Err(ApiError::Invalid);
        }
        if !self.backend.supports_tts() {
            return Err(ApiError::Unavailable);
        }
        let _permit = self
            .tts
            .clone()
            .try_acquire_owned()
            .map_err(|_| ApiError::Capacity)?;
        if self.memory.lock().expect("pilot state").previews.len() >= 8 {
            return Err(ApiError::Capacity);
        }
        let audio = self
            .provider_wait(
                generation,
                Duration::from_secs(30),
                self.backend.synthesize(text.clone(), language),
            )
            .await?
            .map_err(|_| ApiError::Unavailable)?;
        validate_audio(&audio)?;
        self.positive(generation)?;
        let id = auth::secret().map_err(|_| ApiError::Unavailable)?;
        let digest = auth::hex_hash(
            &serde_json::to_vec(
                &json!({"audio_sha256":auth::hex_hash(&audio.wav),"language":language,"text":text}),
            )
            .map_err(|_| ApiError::Unavailable)?,
        );
        let view = Preview {
            id: id.clone(),
            generation,
            text,
            language,
            digest,
            audio_path: format!("/api/previews/{id}/audio"),
            duration_ms: audio.duration_ms,
            auditioned: false,
            state: "prepared",
        };
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if memory
            .previews
            .values()
            .map(|c| c.audio.len())
            .sum::<usize>()
            + audio.wav.len()
            > 16 * 1024 * 1024
        {
            return Err(ApiError::Capacity);
        }
        memory.previews.insert(
            id,
            Clip {
                view: view.clone(),
                audio: Arc::new(audio.wav),
            },
        );
        Ok(view)
    }
    pub fn audition(&self, generation: u64, id: &str, digest: &str) -> Result<Preview, ApiError> {
        self.positive(generation)?;
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        let clip = memory.previews.get_mut(id).ok_or(ApiError::Invalid)?;
        if clip.view.digest != digest || clip.view.state != "prepared" {
            return Err(ApiError::Conflict);
        }
        clip.view.auditioned = true;
        Ok(clip.view.clone())
    }
    pub fn preview_audio(&self, id: &str) -> Result<Arc<Vec<u8>>, ApiError> {
        self.positive(self.generation())?;
        self.memory
            .lock()
            .expect("pilot state")
            .previews
            .get(id)
            .map(|c| c.audio.clone())
            .ok_or(ApiError::Invalid)
    }
    pub fn audio_current(&self, session: &str, role: Role, id: &str) -> bool {
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return false;
        }
        let memory = self.memory.lock().expect("pilot state");
        match role {
            Role::Operator => {
                memory.operator.as_ref().is_some_and(|s| s.id == session)
                    && memory.previews.contains_key(id)
            }
            Role::Public => {
                memory.viewer.as_ref().is_some_and(|s| s.id == session)
                    && memory.public_connection.as_ref().is_some_and(|c| {
                        c.session == session && c.heartbeat.elapsed() < Duration::from_secs(3)
                    })
                    && memory.delivery.as_ref().is_some_and(|d| {
                        d.viewer == session && d.id == id && Instant::now() < d.deadline
                    })
            }
        }
    }
    pub fn approve(
        self: &Arc<Self>,
        generation: u64,
        id: &str,
        digest: &str,
    ) -> Result<String, ApiError> {
        self.positive(generation)?;
        let delivery_id = auth::secret().map_err(|_| ApiError::Unavailable)?;
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if memory
            .delivery
            .as_ref()
            .is_some_and(|d| Instant::now() < d.deadline)
        {
            return Err(ApiError::Conflict);
        }
        Self::unknown_delivery(&mut memory);
        let viewer = memory
            .public_connection
            .as_ref()
            .filter(|c| c.heartbeat.elapsed() < Duration::from_secs(3))
            .ok_or(ApiError::Unavailable)?;
        let viewer_id = viewer.session.clone();
        let sender = viewer.events.clone();
        let clip = memory.previews.get_mut(id).ok_or(ApiError::Invalid)?;
        if clip.view.digest != digest || !clip.view.auditioned || clip.view.state != "prepared" {
            return Err(ApiError::Conflict);
        }
        clip.view.state = "offered";
        let event = json!({"type":"delivery","generation":generation,"delivery_id":delivery_id,"text":clip.view.text,"language":clip.view.language,"digest":clip.view.digest,"audio_path":format!("/api/public/audio/{delivery_id}"),"duration_ms":clip.view.duration_ms});
        let duration = Duration::from_millis(clip.view.duration_ms + 5000);
        memory.delivery = Some(Delivery {
            id: delivery_id.clone(),
            preview: id.to_owned(),
            viewer: viewer_id,
            deadline: self.operation_deadline(duration),
            fetched: false,
            acknowledged: false,
        });
        if sender.try_send(event).is_err() {
            Self::unknown_delivery(&mut memory);
            return Err(ApiError::Unavailable);
        }
        drop(memory);
        let pilot = Arc::clone(self);
        let expected = delivery_id.clone();
        tokio::spawn(async move {
            tokio::time::sleep(Duration::from_secs(3)).await;
            let mut memory = pilot.memory.lock().expect("pilot state");
            if memory
                .delivery
                .as_ref()
                .is_some_and(|d| d.id == expected && !d.acknowledged)
            {
                Self::unknown_delivery(&mut memory);
                drop(memory);
                pilot.emit_private(json!({"type":"receipt","generation":pilot.generation(),"delivery_id":expected,"state":"unknown","meaning":"local_browser_report_only"}));
            }
        });
        Ok(delivery_id)
    }
    pub fn public_audio(&self, session: &str, id: &str) -> Result<Arc<Vec<u8>>, ApiError> {
        self.positive(self.generation())?;
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if !memory
            .public_connection
            .as_ref()
            .is_some_and(|c| c.session == session && c.heartbeat.elapsed() < Duration::from_secs(3))
        {
            return Err(ApiError::Unauthorized);
        }
        let delivery = memory
            .delivery
            .as_mut()
            .filter(|d| {
                d.id == id && d.viewer == session && Instant::now() < d.deadline && !d.fetched
            })
            .ok_or(ApiError::Forbidden)?;
        delivery.fetched = true;
        let preview = delivery.preview.clone();
        memory
            .previews
            .get(&preview)
            .map(|c| c.audio.clone())
            .ok_or(ApiError::Unavailable)
    }
    pub fn receipt(
        &self,
        session: &str,
        generation: u64,
        id: &str,
        state: &str,
    ) -> Result<(), ApiError> {
        self.positive(generation)?;
        if !["accepted", "playback_started", "playback_ended", "failed"].contains(&state) {
            return Err(ApiError::Invalid);
        }
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.finishing.load(Ordering::Acquire) || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if memory
            .public_connection
            .as_ref()
            .is_none_or(|c| c.session != session || c.heartbeat.elapsed() >= Duration::from_secs(3))
        {
            return Err(ApiError::Unauthorized);
        }
        let delivery = memory
            .delivery
            .as_mut()
            .filter(|d| {
                d.id == id
                    && d.viewer == session
                    && Instant::now() < d.deadline
                    && (d.fetched || state == "failed")
            })
            .ok_or(ApiError::Forbidden)?;
        let preview = delivery.preview.clone();
        let clip = memory
            .previews
            .get_mut(&preview)
            .ok_or(ApiError::Unavailable)?;
        let next = match (clip.view.state, state) {
            ("offered", "accepted") => "accepted",
            ("accepted", "playback_started") => "playback_started",
            ("playback_started", "playback_ended") => "playback_ended",
            ("offered" | "accepted" | "playback_started", "failed") => "failed",
            (current, next) if current == next => current,
            _ => return Err(ApiError::Conflict),
        };
        clip.view.state = next;
        if let Some(delivery) = memory.delivery.as_mut() {
            delivery.acknowledged = true;
        }
        if next == "playback_ended" || next == "failed" {
            memory.delivery = None;
        }
        drop(memory);
        self.emit_private(json!({"type":"receipt","generation":generation,"delivery_id":id,"state":state,"meaning":"local_browser_report_only"}));
        Ok(())
    }
    fn unknown_delivery(memory: &mut Memory) {
        if let Some(delivery) = memory.delivery.take()
            && let Some(clip) = memory.previews.get_mut(&delivery.preview)
        {
            clip.view.state = "unknown";
        }
    }
}

pub fn bounded_text(text: &str) -> Result<(), ApiError> {
    if text.trim().is_empty() || text.chars().count() > 2000 || text.contains('\0') {
        Err(ApiError::Invalid)
    } else {
        Ok(())
    }
}

fn validate_audio(audio: &crate::engine::PreparedAudio) -> Result<(), ApiError> {
    if audio.wav.len() < 44
        || audio.wav.len() > 4 * 1024 * 1024
        || !(1..=60000).contains(&audio.duration_ms)
        || ![16000, 24000].contains(&audio.sample_rate)
        || &audio.wav[..4] != b"RIFF"
        || &audio.wav[8..12] != b"WAVE"
    {
        return Err(ApiError::Unavailable);
    }
    let u32_at = |offset: usize| {
        u32::from_le_bytes(
            audio.wav[offset..offset + 4]
                .try_into()
                .expect("bounded WAV field"),
        )
    };
    if u32_at(4) as usize != audio.wav.len() - 8 {
        return Err(ApiError::Unavailable);
    }
    let mut offset = 12usize;
    let mut format = false;
    let mut data = None;
    while offset + 8 <= audio.wav.len() {
        let size = u32_at(offset + 4) as usize;
        let start = offset + 8;
        let end = start.checked_add(size).ok_or(ApiError::Unavailable)?;
        if end > audio.wav.len() {
            return Err(ApiError::Unavailable);
        }
        match &audio.wav[offset..offset + 4] {
            b"fmt " => {
                if format
                    || size < 16
                    || audio.wav[start..start + 4] != [1, 0, 1, 0]
                    || u32_at(start + 4) != audio.sample_rate
                    || u32_at(start + 8) != audio.sample_rate * 2
                    || audio.wav[start + 12..start + 16] != [2, 0, 16, 0]
                {
                    return Err(ApiError::Unavailable);
                }
                format = true;
            }
            b"data" => {
                if data.is_some() || size == 0 || !size.is_multiple_of(2) {
                    return Err(ApiError::Unavailable);
                }
                data = Some(size);
            }
            _ => {}
        }
        offset = end + size % 2;
    }
    let size = data.ok_or(ApiError::Unavailable)?;
    let actual_ms = size as u64 * 1000 / (u64::from(audio.sample_rate) * 2);
    if !format || offset != audio.wav.len() || actual_ms.abs_diff(audio.duration_ms) > 1 {
        return Err(ApiError::Unavailable);
    }
    Ok(())
}

#[cfg(test)]
mod persistent_tests;
