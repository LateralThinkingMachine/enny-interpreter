//! Explicit warning-only continuous operation over independently bounded windows.
//! No owned window overlaps its predecessor. Planned handover preserves source
//! completion; classified interruptions recover with explicit incomplete coverage.
use crate::{
    budget::{BudgetSnapshot, BudgetState, ContinuousSpending, Reservations, stream_cost},
    engine::{
        DrainCompletion, Language, LiveMode, PreparedAudio, ProviderBackend, ProviderError,
        ProviderFailure, ProviderFuture, Route, StreamEvent, StreamHandle, StreamInput,
    },
    provider::{OpenAiBackend, ProviderConfig},
};
use serde::Deserialize;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    sync::{mpsc, watch},
    task::JoinHandle,
    time::Instant,
};

const MAX_SAFE: u64 = (1 << 53) - 1;
const CLOSE_SECONDS: u64 = 12;
fn handover_margin(route: Route) -> u64 {
    if route == Route::Transcription {
        175
    } else {
        45
    }
}

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ContinuousPolicy {
    pub route_duration_seconds: u64,
    pub max_concurrent_sessions: u32,
    pub max_input_audio_seconds: u64,
    pub max_output_audio_seconds: u64,
    pub max_text_characters: u64,
    pub warning_threshold_microusd: u64,
    pub allowed_routes: Vec<Route>,
    pub allowed_live_modes: Vec<LiveMode>,
}
impl ContinuousPolicy {
    fn validate(&self) -> Result<(), ProviderError> {
        if self.route_duration_seconds != 300
            || self.max_concurrent_sessions != 2
            || self.max_input_audio_seconds != 600
            || self.max_output_audio_seconds != 600
            || self.max_text_characters != 76800
            || !(10000..=1_000_000_000).contains(&self.warning_threshold_microusd)
            || self.allowed_routes != [Route::Transcription, Route::Live]
            || self.allowed_live_modes != [LiveMode::Interpretation]
        {
            return Err(ProviderError::Protocol);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Default)]
struct Retirement {
    released: bool,
    confirmed: bool,
    remote_closed: bool,
}
type Shutdown<'a> = Pin<Box<dyn Future<Output = Retirement> + Send + 'a>>;
trait WindowOwner: Send + Sync {
    fn backend(&self) -> Arc<dyn ProviderBackend>;
    fn shutdown(&self) -> Shutdown<'_>;
}
struct OpenAiWindow(Arc<OpenAiBackend>);
impl WindowOwner for OpenAiWindow {
    fn backend(&self) -> Arc<dyn ProviderBackend> {
        self.0.clone()
    }
    fn shutdown(&self) -> Shutdown<'_> {
        Box::pin(async {
            let released = self.0.retire().await;
            Retirement {
                released,
                confirmed: self.0.budget_snapshot().uncertain == Some(false),
                remote_closed: self.0.closure_confirmed(),
            }
        })
    }
}
type Factory = Arc<
    dyn Fn(
            Route,
            Vec<watch::Receiver<bool>>,
            watch::Receiver<bool>,
        ) -> Result<Arc<dyn WindowOwner>, ProviderError>
        + Send
        + Sync,
>;

#[derive(Default)]
struct Ledger {
    active: [bool; 2],
    allocated: u64,
    windows: u64,
    uncertain: bool,
    retirement_uncertain: bool,
    closure_uncertain: bool,
    failure: Option<ProviderFailure>,
    tasks: Vec<JoinHandle<()>>,
}
#[derive(Default)]
pub(crate) struct ProcessAllocations(Mutex<(u64, u64)>);

impl ProcessAllocations {
    pub(crate) fn snapshot(&self) -> Option<(u64, u64)> {
        self.0.lock().ok().map(|v| *v)
    }
    pub(crate) fn reserve_external_live(&self, seconds: u64) -> Result<(), ProviderError> {
        let cost = stream_cost(Route::Live, seconds).ok_or(ProviderError::Budget)?;
        let mut ledger = self.0.lock().map_err(|_| ProviderError::Unavailable)?;
        let allocated = ledger
            .0
            .checked_add(cost)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(ProviderError::Budget)?;
        let windows = ledger
            .1
            .checked_add(1)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(ProviderError::Budget)?;
        *ledger = (allocated, windows);
        Ok(())
    }
}

struct Shared {
    allocations: Arc<ProcessAllocations>,
    policy: ContinuousPolicy,
    factory: Factory,
    ledger: Mutex<Ledger>,
    stop: watch::Sender<bool>,
}
pub struct ContinuousBackend {
    shared: Arc<Shared>,
    retirement: tokio::sync::Mutex<()>,
}

struct ShutdownTasks {
    shared: Arc<Shared>,
    tasks: Vec<JoinHandle<()>>,
    completed: bool,
}
impl Drop for ShutdownTasks {
    fn drop(&mut self) {
        if !self.completed {
            self.shared.poison();
            self.shared.retirement_failed();
            for task in &self.tasks {
                task.abort();
            }
        }
    }
}

fn slot(route: Route) -> Result<usize, ProviderError> {
    match route {
        Route::Transcription => Ok(0),
        Route::Live => Ok(1),
        _ => Err(ProviderError::Unavailable),
    }
}
impl ContinuousBackend {
    pub fn new(api_key: String, policy: ContinuousPolicy) -> Result<Self, ProviderError> {
        Self::new_with_allocations(api_key, policy, Arc::new(ProcessAllocations::default()))
    }
    pub(crate) fn new_with_allocations(
        api_key: String,
        policy: ContinuousPolicy,
        allocations: Arc<ProcessAllocations>,
    ) -> Result<Self, ProviderError> {
        policy.validate()?;
        if !crate::provider::valid_key(crate::budget::ProviderVendor::OpenAi, &api_key) {
            return Err(ProviderError::Protocol);
        }
        let window_policy = policy.clone();
        let factory: Factory = Arc::new(move |route, parents, admission_fence| {
            let p = &window_policy;
            let config = ProviderConfig {
                api_key: api_key.clone(),
                allowed_routes: vec![route],
                allowed_live_modes: if route == Route::Live {
                    vec![LiveMode::Interpretation]
                } else {
                    vec![]
                },
                allow_tts: false,
                total_duration_seconds: p.route_duration_seconds,
                route_duration_seconds: p.route_duration_seconds,
                max_sessions: 1,
                max_tts_requests: 0,
                max_input_audio_seconds: p.max_input_audio_seconds,
                max_output_audio_seconds: p.max_output_audio_seconds,
                max_text_characters: p.max_text_characters,
                approved_budget_microusd: stream_cost(route, p.route_duration_seconds)
                    .ok_or(ProviderError::Budget)?,
            };
            Ok(Arc::new(OpenAiWindow(Arc::new(
                OpenAiBackend::new_with_controls(config, parents, Some(admission_fence))?,
            ))))
        });
        Ok(Self::with_factory_allocations(policy, factory, allocations))
    }
    #[cfg(test)]
    fn with_factory(policy: ContinuousPolicy, factory: Factory) -> Self {
        Self::with_factory_allocations(policy, factory, Arc::new(ProcessAllocations::default()))
    }
    fn with_factory_allocations(
        policy: ContinuousPolicy,
        factory: Factory,
        allocations: Arc<ProcessAllocations>,
    ) -> Self {
        let (stop, _) = watch::channel(false);
        Self {
            retirement: tokio::sync::Mutex::new(()),
            shared: Arc::new(Shared {
                allocations,
                policy,
                factory,
                ledger: Mutex::new(Ledger::default()),
                stop,
            }),
        }
    }
    pub(crate) fn closure_confirmed(&self) -> bool {
        self.shared.ledger.lock().is_ok_and(|l| {
            !l.retirement_uncertain && !l.closure_uncertain && !l.active.iter().any(|a| *a)
        })
    }
    pub async fn shutdown(&self) {
        let _ = self.retire().await;
    }
    pub(crate) async fn retire(&self) -> bool {
        // All callers observe the same completed retirement, never an empty
        // task registry while another caller is still joining those tasks.
        let _retirement = self.retirement.lock().await;
        self.shared.stop.send_replace(true);
        let tasks = match self.shared.ledger.lock() {
            Ok(mut ledger) => std::mem::take(&mut ledger.tasks),
            Err(_) => return false,
        };
        let mut owned = ShutdownTasks {
            shared: self.shared.clone(),
            tasks,
            completed: false,
        };
        let end = Instant::now() + Duration::from_secs(CLOSE_SECONDS + 1);
        for task in &mut owned.tasks {
            match tokio::time::timeout_at(end, &mut *task).await {
                Ok(Ok(())) => {}
                Ok(Err(_)) => {
                    self.shared.poison();
                    self.shared.retirement_failed();
                }
                Err(_) => {
                    self.shared.poison();
                    self.shared.retirement_failed();
                    task.abort();
                    let _ = task.await;
                }
            }
        }
        owned.completed = true;
        self.shared
            .ledger
            .lock()
            .is_ok_and(|ledger| !ledger.retirement_uncertain)
    }
}
impl Drop for ContinuousBackend {
    fn drop(&mut self) {
        self.shared.stop.send_replace(true);
    }
}
impl Shared {
    fn retirement_failed(&self) {
        if let Ok(mut ledger) = self.ledger.lock() {
            ledger.retirement_uncertain = true;
        }
    }
    fn remember_failure(&self, route: Route, error: ProviderError) -> ProviderFailure {
        let failure = ProviderFailure { route, error };
        match self.ledger.lock() {
            Ok(mut ledger) => {
                // The cause becomes visible at the same instant admission is
                // revoked; publishing it does not create an admission window
                // before the subsequent sibling-cancellation signal.
                ledger.uncertain = true;
                *ledger.failure.get_or_insert(failure)
            }
            Err(_) => failure,
        }
    }
    fn available(&self) -> bool {
        !*self.stop.borrow() && self.ledger.lock().is_ok_and(|l| !l.uncertain)
    }
    fn mark_uncertain(&self) {
        if let Ok(mut l) = self.ledger.lock() {
            l.uncertain = true;
        }
    }
    fn poison(&self) {
        if let Ok(mut l) = self.ledger.lock() {
            l.uncertain = true;
        }
        self.stop.send_replace(true);
    }
    fn allocate(&self, route: Route) -> Result<(), ProviderError> {
        let mut l = self.ledger.lock().map_err(|_| ProviderError::Unavailable)?;
        if *self.stop.borrow() || l.uncertain {
            return Err(ProviderError::Unavailable);
        }
        let cost =
            stream_cost(route, self.policy.route_duration_seconds).ok_or(ProviderError::Budget)?;
        let allocated = l
            .allocated
            .checked_add(cost)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(ProviderError::ProtocolCapacityLimit)?;
        let windows = l
            .windows
            .checked_add(1)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(ProviderError::ProtocolCapacityLimit)?;
        let mut process = self
            .allocations
            .0
            .lock()
            .map_err(|_| ProviderError::Unavailable)?;
        let process_allocated = process
            .0
            .checked_add(cost)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(ProviderError::ProtocolCapacityLimit)?;
        let process_windows = process
            .1
            .checked_add(1)
            .filter(|v| *v <= MAX_SAFE)
            .ok_or(ProviderError::ProtocolCapacityLimit)?;
        // Neither a new meeting nor a failed attempt resets process estimates.
        *process = (process_allocated, process_windows);
        // Monotonic conservative allocations; warnings are never admission gates.
        l.allocated = allocated;
        l.windows = windows;
        Ok(())
    }
}
impl ProviderBackend for ContinuousBackend {
    fn revoke(&self) {
        // Serialize the fence with window reservation, as well as checking it
        // directly before every child transport poll.
        let _ledger = self.shared.ledger.lock();
        self.shared.stop.send_replace(true);
    }
    fn failure_cause(&self) -> Option<ProviderFailure> {
        self.shared
            .ledger
            .lock()
            .ok()
            .and_then(|ledger| ledger.failure)
    }
    fn enabled(&self) -> bool {
        self.shared.available()
    }
    fn supports(&self, route: Route) -> bool {
        slot(route).is_ok() && self.enabled()
    }
    fn supports_tts(&self) -> bool {
        false
    }
    fn supports_live_mode(&self, mode: LiveMode) -> bool {
        mode == LiveMode::Interpretation && self.enabled()
    }
    fn budget_snapshot(&self) -> BudgetSnapshot {
        let Ok(l) = self.shared.ledger.lock() else {
            return BudgetSnapshot::inactive(BudgetState::Unavailable);
        };
        let stopped = *self.shared.stop.borrow();
        let state = if l.uncertain {
            BudgetState::Uncertain
        } else if stopped {
            BudgetState::Stopped
        } else {
            BudgetState::Active
        };
        let mut s = BudgetSnapshot::inactive(state);
        s.uncertain = Some(l.uncertain);
        s.remaining_seconds = None;
        s.route_duration_seconds = Some(self.shared.policy.route_duration_seconds);
        let Ok(process) = self.shared.allocations.0.lock() else {
            return BudgetSnapshot::inactive(BudgetState::Unavailable);
        };
        s.continuous = Some(ContinuousSpending {
            warning_threshold_microusd: self.shared.policy.warning_threshold_microusd,
            allocated_microusd: process.0,
            windows_started: process.1,
        });
        s.stream_reservations = Some(Reservations {
            used: l.active.iter().filter(|v| **v).count() as u32,
            limit: 2,
        });
        s.tts_reservations = Some(Reservations { used: 0, limit: 0 });
        for q in &mut s.routes {
            if let Ok(index) = slot(q.route) {
                q.configured = true;
                q.quote_seconds = Some(self.shared.policy.route_duration_seconds);
                q.quote_microusd = stream_cost(q.route, self.shared.policy.route_duration_seconds);
                q.can_reserve = !stopped && !l.uncertain && !l.active[index];
            }
        }
        s
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
        self.open_options(
            route,
            language,
            crate::engine::RouteOptions {
                live_mode: mode,
                transcription: None,
                interpretation: None,
            },
        )
    }
    fn open_interpretation(
        &self,
        language: Language,
        policy: crate::engine::Interpretation,
    ) -> ProviderFuture<StreamHandle> {
        self.open_options(
            Route::Live,
            language,
            crate::engine::RouteOptions {
                live_mode: Some(LiveMode::Interpretation),
                transcription: None,
                interpretation: Some(policy),
            },
        )
    }
    fn open_transcription(
        &self,
        language: Language,
        hints: crate::engine::SpeechHints,
    ) -> ProviderFuture<StreamHandle> {
        self.open_options(
            Route::Transcription,
            language,
            crate::engine::RouteOptions {
                live_mode: None,
                transcription: Some(hints),
                interpretation: None,
            },
        )
    }
    fn open_interpretation_context(
        &self,
        language: Language,
        policy: crate::engine::Interpretation,
        context: crate::interpretation_context::InterpretationContext,
    ) -> ProviderFuture<StreamHandle> {
        self.open_options_context(
            Route::Live,
            language,
            crate::engine::RouteOptions {
                live_mode: Some(LiveMode::Interpretation),
                interpretation: Some(policy),
                ..Default::default()
            },
            context,
        )
    }
    fn synthesize(&self, _: String, _: Language) -> ProviderFuture<PreparedAudio> {
        Box::pin(async { Err(ProviderError::Unavailable) })
    }
}

impl ContinuousBackend {
    fn open_options(
        &self,
        route: Route,
        language: Language,
        options: crate::engine::RouteOptions,
    ) -> ProviderFuture<StreamHandle> {
        self.open_options_context(route, language, options, Default::default())
    }
    fn open_options_context(
        &self,
        route: Route,
        language: Language,
        options: crate::engine::RouteOptions,
        context: crate::interpretation_context::InterpretationContext,
    ) -> ProviderFuture<StreamHandle> {
        let shared = self.shared.clone();
        Box::pin(async move {
            context.validate()?;
            if options.interpretation.is_none() && context != Default::default() {
                return Err(ProviderError::Protocol);
            }
            let index = slot(route)?;
            let mode = route.resolve_live_mode(options.live_mode)?;
            if let Some(policy) = options.interpretation {
                policy.validate(language)?;
                if route != Route::Live || mode != Some(LiveMode::Interpretation) {
                    return Err(ProviderError::Protocol);
                }
            }
            if let Some(hints) = &options.transcription {
                if route != Route::Transcription {
                    return Err(ProviderError::Protocol);
                }
                hints.validate()?;
            }
            if !language.legacy()
                && options.interpretation.is_none()
                && (route != Route::Transcription
                    || options
                        .transcription
                        .as_ref()
                        .is_none_or(|hints| !hints.languages.contains(&language)))
            {
                return Err(ProviderError::Protocol);
            }
            if route == Route::Live && mode != Some(LiveMode::Interpretation) {
                return Err(ProviderError::Unavailable);
            }
            let (input, input_rx) = mpsc::channel(8);
            let (events, event_rx) = mpsc::channel(32);
            let (stop, stop_rx) = watch::channel(false);
            let (renewal_fence, fence_rx) = watch::channel(false);
            let mut ledger = shared
                .ledger
                .lock()
                .map_err(|_| ProviderError::Unavailable)?;
            if *shared.stop.borrow() || ledger.uncertain {
                return Err(ProviderError::Unavailable);
            }
            ledger.tasks.retain(|task| !task.is_finished());
            if ledger.active[index] || ledger.tasks.len() >= 2 {
                return Err(ProviderError::StreamLimit);
            }
            ledger.active[index] = true;
            let task_shared = shared.clone();
            ledger.tasks.push(tokio::spawn(async move {
                supervise(
                    task_shared.clone(),
                    route,
                    language,
                    crate::engine::RouteOptions {
                        live_mode: mode,
                        transcription: options.transcription,
                        interpretation: options.interpretation,
                    },
                    context,
                    input_rx,
                    events,
                    stop_rx,
                    fence_rx,
                )
                .await;
                if let Ok(mut l) = task_shared.ledger.lock() {
                    l.active[index] = false;
                }
            }));
            Ok(StreamHandle {
                input,
                events: event_rx,
                stop,
                renewal_fence: Some(renewal_fence),
            })
        })
    }
}

enum Outcome {
    Renew(DrainCompletion),
    Finished,
    Stopped,
    Fault(ProviderError),
}
fn publish(events: &mpsc::Sender<StreamEvent>, event: StreamEvent) -> Result<(), ProviderError> {
    events
        .try_send(event)
        .map_err(|_| ProviderError::ProtocolCapacityLimit)
}

/// During bounded opening/cleanup, consume and discard transition PCM. This
/// prevents the outer eight-frame queue from turning a slow close into Stop.
#[allow(clippy::too_many_arguments)]
async fn transition<F: Future>(
    future: F,
    limit: Duration,
    input: &mut mpsc::Receiver<StreamInput>,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    finish: &mut bool,
    finish_ends: bool,
    admission: Option<&watch::Receiver<bool>>,
) -> Result<Option<F::Output>, ProviderError> {
    tokio::pin!(future);
    let mut admission_watch = admission.cloned();
    let checked = std::future::poll_fn(|cx| {
        if admission.is_some_and(|f| *f.borrow() || f.has_changed().is_err()) {
            std::task::Poll::Ready(Err(ProviderError::SourceIncomplete))
        } else {
            future.as_mut().poll(cx).map(Ok)
        }
    });
    tokio::pin!(checked);
    let timer = tokio::time::sleep(limit);
    tokio::pin!(timer);
    loop {
        if *stop.borrow() || *global.borrow() {
            return Ok(None);
        }
        tokio::select! { biased;
            _=stop.changed()=>return Ok(None), _=global.changed()=>return Ok(None),
            _=&mut timer=>return Err(ProviderError::Deadline),
            _=async { match admission_watch.as_mut() { Some(f)=>{ let _=f.changed().await; }, None=>std::future::pending::<()>().await } }=>return Err(ProviderError::SourceIncomplete),
            message=input.recv()=>match message {
                Some(StreamInput::Pcm16(bytes)) if bytes.len() == 640 && !*finish => {},
                Some(StreamInput::FinishInput) if !*finish => { *finish = true; if finish_ends { return Ok(None); } },
                None=>return Ok(None),
                _=>return Err(ProviderError::Protocol),
            },
            result=&mut checked=>return result.map(Some),
        }
    }
}

async fn cleanup(
    owner: &dyn WindowOwner,
    input: &mut mpsc::Receiver<StreamInput>,
    finish: &mut bool,
) -> Retirement {
    let future = owner.shutdown();
    tokio::pin!(future);
    let timeout = tokio::time::sleep(Duration::from_secs(CLOSE_SECONDS));
    tokio::pin!(timeout);
    let mut input_open = true;
    let mut valid = true;
    loop {
        tokio::select! { biased;
            _=&mut timeout=>return Retirement::default(),
            result=&mut future=>return Retirement { released: result.released && valid, confirmed: result.confirmed && valid, remote_closed: result.remote_closed },
            message=input.recv(), if input_open=>match message {
                Some(StreamInput::Pcm16(bytes)) if bytes.len() == 640 && !*finish=>{},
                Some(StreamInput::FinishInput) if !*finish=>*finish=true,
                None=>input_open=false,
                _=>valid=false,
            },
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn supervise(
    shared: Arc<Shared>,
    route: Route,
    language: Language,
    options: crate::engine::RouteOptions,
    context: crate::interpretation_context::InterpretationContext,
    mut input: mpsc::Receiver<StreamInput>,
    events: mpsc::Sender<StreamEvent>,
    mut stop: watch::Receiver<bool>,
    fence: watch::Receiver<bool>,
) {
    let mut global = shared.stop.subscribe();
    let mut window = 1u64;
    let mut terminal = Outcome::Stopped;
    let mut finish = false;
    let mut previous = None;
    let mut failure_published = false;
    let mut retry = RetryCadence::default();
    loop {
        if *stop.borrow() || *global.borrow() {
            break;
        }
        // Drain already queued input before creating another paid window.
        while let Ok(message) = input.try_recv() {
            match message {
                StreamInput::Pcm16(bytes) if bytes.len() == 640 && !finish => {}
                StreamInput::FinishInput if !finish => finish = true,
                _ => {
                    terminal = Outcome::Fault(ProviderError::Protocol);
                    break;
                }
            }
        }
        if matches!(terminal, Outcome::Fault(_)) {
            break;
        }
        if *fence.borrow() && !finish {
            // The synchronous runtime fence precedes the ordered source barrier.
            // Wait for that barrier with no new provider admission.
            match transition(
                std::future::pending::<()>(),
                Duration::from_secs(30),
                &mut input,
                &mut stop,
                &mut global,
                &mut finish,
                true,
                None,
            )
            .await
            {
                Err(ProviderError::Deadline) if finish => {}
                Err(e) => {
                    terminal = Outcome::Fault(e);
                    break;
                }
                _ if !finish => break,
                _ => {}
            }
        }
        if finish {
            terminal = if let Some(completion) = previous {
                match publish(&events, StreamEvent::Drained { completion }) {
                    Ok(()) => Outcome::Finished,
                    Err(e) => Outcome::Fault(e),
                }
            } else {
                Outcome::Fault(ProviderError::SourceIncomplete)
            };
            break;
        }
        let owner = match (shared.factory)(route, vec![stop.clone(), global.clone()], fence.clone())
            .and_then(|owner| {
                if *fence.borrow() {
                    return Err(ProviderError::SourceIncomplete);
                }
                shared.allocate(route)?;
                Ok(owner)
            }) {
            Ok(owner) => owner,
            Err(error) => {
                terminal = Outcome::Fault(error);
                break;
            }
        };
        let backend = owner.backend();
        let attempt_started = Instant::now();
        let opened = transition(
            match route {
                Route::Transcription => backend.open_transcription(
                    language,
                    options.transcription.clone().unwrap_or_default(),
                ),
                Route::Live if options.interpretation.is_some() => backend
                    .open_interpretation_context(
                        language,
                        options.interpretation.expect("checked"),
                        context.clone(),
                    ),
                _ => backend.open_with_mode(route, language, options.live_mode),
            },
            Duration::from_secs(5),
            &mut input,
            &mut stop,
            &mut global,
            &mut finish,
            false,
            Some(&fence),
        )
        .await;
        terminal = match opened {
            Ok(Some(Ok(mut handle))) => {
                let result = if finish {
                    Outcome::Fault(ProviderError::SourceIncomplete)
                } else {
                    drive_window(
                        route,
                        &mut handle,
                        &mut input,
                        &events,
                        &mut stop,
                        &mut global,
                        &fence,
                    )
                    .await
                };
                handle.stop.send_replace(true);
                result
            }
            Ok(Some(Err(error))) | Err(error) => Outcome::Fault(error),
            Ok(None) => Outcome::Stopped,
        };
        // A recoverable interruption stays within the same explicit Start.
        // Publish the next-window gap before waiting for cleanup; late child
        // content remains owned by the retired channel and is never forwarded.
        let finishing_cancel = matches!(terminal, Outcome::Stopped) && *fence.borrow();
        let recovering = match terminal {
            Outcome::Fault(error)
                if error.recoverable()
                    && !finish
                    && !*fence.borrow()
                    && !*stop.borrow()
                    && !*global.borrow() =>
            {
                let next = window.checked_add(1).filter(|v| *v <= MAX_SAFE);
                if let Some(next) = next {
                    let delay = retry.delay(attempt_started.elapsed());
                    match publish(
                        &events,
                        StreamEvent::Recovering {
                            window: next,
                            error,
                            retry_in_seconds: delay,
                        },
                    ) {
                        Ok(()) => Some((next, delay)),
                        Err(error) => {
                            terminal = Outcome::Fault(error);
                            None
                        }
                    }
                } else {
                    terminal = Outcome::Fault(ProviderError::ProtocolCapacityLimit);
                    None
                }
            }
            _ => None,
        };
        if recovering.is_none() {
            if let Outcome::Fault(error) = terminal {
                shared.remember_failure(route, error);
                failure_published = publish(&events, StreamEvent::Error(error)).is_ok();
            }
            if matches!(terminal, Outcome::Fault(_) | Outcome::Stopped) {
                if finishing_cancel {
                    shared.mark_uncertain();
                } else {
                    shared.poison();
                }
            }
        }
        // This future is never cancelled by Stop. All local transports must be
        // joined (or abort-and-joined) before another child can be admitted.
        let retired = cleanup(owner.as_ref(), &mut input, &mut finish).await;
        if !retired.remote_closed
            && let Ok(mut ledger) = shared.ledger.lock()
        {
            ledger.closure_uncertain = true;
        }
        if !retired.released {
            shared.retirement_failed();
        }
        if !retired.released || (recovering.is_none() && !retired.confirmed) {
            if !matches!(terminal, Outcome::Fault(_)) || recovering.is_some() {
                terminal = Outcome::Fault(ProviderError::Unavailable);
            }
            if !finishing_cancel {
                let Outcome::Fault(error) = terminal else {
                    unreachable!("cleanup failure is terminal")
                };
                shared.remember_failure(route, error);
                if !failure_published {
                    failure_published = publish(&events, StreamEvent::Error(error)).is_ok();
                }
            }
            if finishing_cancel {
                shared.mark_uncertain();
            } else {
                shared.poison();
            }
        }
        if *stop.borrow() || *global.borrow() {
            break;
        }
        if let Some((next, delay)) = recovering {
            // Final usage may be unknown. Its allocation remains charged in our
            // monotonic estimate; interruption never manufactures a final or a
            // refund. Only the warning-only policy permits this recovery path.
            previous = None;
            window = next;
            if finish || *fence.borrow() {
                terminal = Outcome::Fault(ProviderError::SourceIncomplete);
                break;
            }
            match transition(
                tokio::time::sleep(Duration::from_secs(delay)),
                Duration::from_secs(delay + 1),
                &mut input,
                &mut stop,
                &mut global,
                &mut finish,
                true,
                Some(&fence),
            )
            .await
            {
                Ok(Some(())) if !finish && !*fence.borrow() => {
                    terminal = Outcome::Stopped;
                    continue;
                }
                Ok(_) if *stop.borrow() || *global.borrow() => {
                    terminal = Outcome::Stopped;
                    break;
                }
                _ => {
                    terminal = Outcome::Fault(ProviderError::SourceIncomplete);
                    break;
                }
            }
        }
        let Outcome::Renew(completion) = terminal else {
            break;
        };
        previous = Some(completion);
        if finish || *fence.borrow() {
            continue;
        }
        let Some(next) = window.checked_add(1).filter(|v| *v <= MAX_SAFE) else {
            terminal = Outcome::Fault(ProviderError::ProtocolCapacityLimit);
            break;
        };
        window = next;
        if let Err(error) = publish(&events, StreamEvent::Renewing { window }) {
            terminal = Outcome::Fault(error);
            break;
        }
    }
    match terminal {
        Outcome::Fault(error) => {
            let finishing_cancel = *fence.borrow() && *stop.borrow();
            if !finishing_cancel {
                shared.remember_failure(route, error);
            }
            if !failure_published {
                let _ = publish(&events, StreamEvent::Error(error));
            }
            if finishing_cancel {
                shared.mark_uncertain();
            } else {
                shared.poison();
            }
            let _ = publish(&events, StreamEvent::Closed { confirmed: false });
        }
        Outcome::Finished => {
            let _ = publish(&events, StreamEvent::Closed { confirmed: true });
        }
        _ => {
            let _ = publish(&events, StreamEvent::Closed { confirmed: false });
        }
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_window(
    route: Route,
    handle: &mut StreamHandle,
    input: &mut mpsc::Receiver<StreamInput>,
    events: &mpsc::Sender<StreamEvent>,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    fence: &watch::Receiver<bool>,
) -> Outcome {
    let result = drive_checked(route, handle, input, events, stop, global, fence).await;
    match result {
        Ok(outcome) => outcome,
        Err(error) => Outcome::Fault(error),
    }
}

#[allow(clippy::too_many_arguments)]
async fn drive_checked(
    route: Route,
    handle: &mut StreamHandle,
    input: &mut mpsc::Receiver<StreamInput>,
    events: &mpsc::Sender<StreamEvent>,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    fence: &watch::Receiver<bool>,
) -> Result<Outcome, ProviderError> {
    let mut ready = false;
    let mut planned = false;
    let mut sealed = false;
    let mut seal_receipt = false;
    let mut closed = false;
    let mut manual = false;
    let mut drained = None;
    let mut end = Instant::now() + Duration::from_secs(10);
    let mut renew = end;
    loop {
        if *stop.borrow() || *global.borrow() {
            return Ok(Outcome::Stopped);
        }
        tokio::select! { biased;
            _=stop.changed()=>return Ok(Outcome::Stopped),
            _=global.changed()=>return Ok(Outcome::Stopped),
            _=tokio::time::sleep_until(end)=>return Err(ProviderError::Deadline),
            _=tokio::time::sleep_until(renew), if ready && !planned && !manual && !*fence.borrow() => {
                if *fence.borrow() { continue; }
                planned = true;
                publish(events, StreamEvent::Handover)?;
                let request = if route == Route::Transcription { StreamInput::FinishAtBoundary } else { sealed = true; StreamInput::FinishInput };
                handle.input.try_send(request).map_err(|_| ProviderError::ProtocolCapacityLimit)?;
            }
            message=input.recv()=>match message {
                Some(StreamInput::Pcm16(bytes)) => {
                    if bytes.len() != 640 || manual { return Err(ProviderError::Protocol); }
                    // Preserve bounded queues; transition audio is discarded,
                    // never stored or replayed into a later semantic window.
                    if ready && !sealed && drained.is_none() {
                        handle.input.try_send(StreamInput::Pcm16(bytes)).map_err(|_| ProviderError::ProtocolCapacityLimit)?;
                    }
                }
                Some(StreamInput::FinishInput) => {
                    if manual { return Err(ProviderError::Protocol); }
                    manual = true;
                    end = end.min(Instant::now() + Duration::from_secs(32));
                    if let Some(completion) = drained { publish(events, StreamEvent::Drained { completion })?; }
                    if closed { return Ok(Outcome::Finished); }
                    if !sealed {
                        handle.input.try_send(StreamInput::FinishInput).map_err(|_| ProviderError::ProtocolCapacityLimit)?;
                        sealed = true;
                    }
                }
                Some(StreamInput::Text(_) | StreamInput::FinishAtBoundary) => return Err(ProviderError::Protocol),
                None => return Ok(Outcome::Stopped),
            },            event=handle.events.recv(), if !closed =>match event {
                Some(StreamEvent::Ready { expires_in_seconds }) => {
                    if ready || expires_in_seconds <= handover_margin(route) || expires_in_seconds > 300 { return Err(ProviderError::Protocol); }
                    ready = true;
                    renew = Instant::now() + Duration::from_secs(expires_in_seconds - handover_margin(route));
                    end = Instant::now() + Duration::from_secs(expires_in_seconds + CLOSE_SECONDS);
                    publish(events, StreamEvent::Ready { expires_in_seconds })?;
                }
                Some(StreamEvent::InputSealed) => {
                    if !planned || seal_receipt || route != Route::Transcription { return Err(ProviderError::Protocol); }
                    sealed = true; seal_receipt = true;
                }
                Some(StreamEvent::Drained { completion }) => {
                    if !ready || (!planned && !manual) || drained.is_some() { return Err(ProviderError::Protocol); }
                    if route == Route::Transcription && completion != DrainCompletion::CommittedTranscriptsFinal { return Err(ProviderError::SourceIncomplete); }
                    if planned && route == Route::Transcription && !sealed { return Err(ProviderError::SourceIncomplete); }
                    drained = Some(completion);
                    if manual { publish(events, StreamEvent::Drained { completion })?; }
                }
                Some(StreamEvent::Closed { confirmed }) => {
                    if !confirmed || drained.is_none() { return Err(if planned || manual { ProviderError::Unavailable } else { ProviderError::ConnectionLost }); }
                    closed = true;
                    if manual { return Ok(Outcome::Finished); }
                    if !planned { return Err(ProviderError::Protocol); }
                    if !*fence.borrow() { return Ok(Outcome::Renew(drained.ok_or(ProviderError::SourceIncomplete)?)); }
                }
                Some(StreamEvent::Error(error)) => return Err(error),
                Some(event @ (StreamEvent::Audio { .. } | StreamEvent::Transcript { .. } | StreamEvent::SourceDraft { .. } | StreamEvent::LiveTranscript { .. })) => {
                    if !ready || drained.is_some() { return Err(ProviderError::Protocol); }
                    publish(events, event)?;
                }
                Some(StreamEvent::Renewing { .. } | StreamEvent::Recovering { .. } | StreamEvent::Handover) => return Err(ProviderError::Protocol),
                None => return Err(if planned || manual { ProviderError::Unavailable } else { ProviderError::ConnectionLost }),
            },

        }
    }
}

/// A bounded attempt rate rather than a lifetime retry count: an outage does
/// not silently end an owner's active meeting. A stable minute resets the
/// short burst; otherwise each attempt after the third waits thirty seconds.
#[derive(Default)]
struct RetryCadence {
    burst: u8,
}
impl RetryCadence {
    fn delay(&mut self, uptime: Duration) -> u64 {
        if uptime >= Duration::from_secs(60) {
            self.burst = 0;
        }
        self.burst = self.burst.saturating_add(1).min(4);
        match self.burst {
            1 => 1,
            2 => 2,
            3 => 4,
            _ => 30,
        }
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod runtime_tests;
