//! Process availability is separate from an individual meeting's sticky Stop.
//! Rollover is explicit, authenticated, idempotent and never resumes capture.
use crate::{
    auth::{self, ApiError, Role},
    config::{ActivatedBackend, ProviderActivation},
    continuous::{ContinuousBackend, ProcessAllocations},
    engine,
    runtime::Pilot,
};
use axum::{Router, http::HeaderMap};
use serde_json::{Value, json};
use std::{
    path::PathBuf,
    sync::{
        Arc, Mutex,
        atomic::{AtomicBool, Ordering},
    },
    time::{Duration, Instant},
};
use tokio::{sync::watch, task::JoinHandle};

mod pause;
pub(crate) use pause::PauseStatus;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Replacement {
    NewSession,
    Resume,
}

const RECEIPT_SECONDS: u64 = 120;
const ROLLOVER_SECONDS: u64 = 16;
type Factory = Arc<dyn Fn() -> Result<Option<ActivatedBackend>, &'static str> + Send + Sync>;

pub(crate) struct Slot {
    pub(crate) pilot: Arc<Pilot>,
    pause: Mutex<PauseStatus>,
    pause_task: Mutex<Option<JoinHandle<()>>>,
    initial_generation: u64,
    provider: Option<ActivatedBackend>,
    operator: Router,
    public: Router,
    #[cfg(test)]
    retirement_gate: Mutex<Option<Arc<tests::RetirementGate>>>,
}

impl Slot {
    pub(crate) fn router(&self, role: Role) -> Router {
        match role {
            Role::Operator => self.operator.clone(),
            Role::Public => self.public.clone(),
        }
    }
    async fn retire(&self) -> bool {
        #[cfg(test)]
        {
            let gate = self.retirement_gate.lock().unwrap().clone();
            if let Some(gate) = gate {
                gate.entered.add_permits(1);
                gate.release.acquire().await.unwrap().forget();
                return gate.success;
            }
        }
        match &self.provider {
            Some(provider) => provider.retire().await,
            None => true,
        }
    }
}

pub(crate) struct Receipt {
    previous_generation: u64,
    replacement: Replacement,
    request_id: String,
    old: Arc<Pilot>,
    next: Arc<Slot>,
    view: Value,
    pub(crate) token: String,
    deadline: Instant,
}
impl Receipt {
    pub(crate) fn value(&self) -> Value {
        json!({"state":"ready", "previous_generation":self.previous_generation, "request_id":self.request_id, "session":self.view})
    }
    fn matches(
        &self,
        generation: u64,
        request_id: &str,
        headers: &HeaderMap,
        replacement: Replacement,
    ) -> bool {
        replacement == self.replacement
            && Instant::now() < self.deadline
            && self.previous_generation == generation
            && self.request_id == request_id
            && self.authenticates_transition(headers)
    }

    /// During handoff the browser can hold either credential with the old
    /// CSRF value. Only receipt replay and explicit logout use this pair.
    fn authenticates_transition(&self, headers: &HeaderMap) -> bool {
        self.old.rollover_csrf(headers)
            && (self.old.authorize(headers, Role::Operator, true).is_ok()
                || self
                    .next
                    .pilot
                    .authorize(headers, Role::Operator, false)
                    .is_ok())
    }
}
type Completion = Option<Result<Arc<Receipt>, ApiError>>;
struct Pending {
    generation: u64,
    replacement: Replacement,
    request_id: String,
    old: Arc<Pilot>,
    receiver: watch::Receiver<Completion>,
    task: Option<JoinHandle<()>>,
}
struct State {
    current: Arc<Slot>,
    receipt: Option<Arc<Receipt>>,
    pending: Option<Pending>,
}

pub struct SessionHost {
    origins: auth::LocalOrigins,
    assets: PathBuf,
    max_audio_seconds: u64,
    repeatable: bool,
    process_started: Instant,
    factory: Factory,
    limits: [crate::http::RequestLimits; 2],
    state: Mutex<State>,
    stopping: AtomicBool,
    pub(crate) webrtc: Arc<crate::webrtc::WebRtc>,
}
impl SessionHost {
    pub fn new(
        provider: Option<ProviderActivation>,
        duration: Option<u64>,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
        assets: PathBuf,
        repeatable: bool,
    ) -> Result<(Arc<Self>, String), &'static str> {
        if repeatable
            && (duration.is_some() || matches!(provider, Some(ProviderActivation::Finite(_))))
        {
            return Err("repeatable_scope_mismatch");
        }
        let allocations = Arc::new(ProcessAllocations::default());
        let process_started = Instant::now();
        let factory: Factory = match (&provider, repeatable) {
            (Some(ProviderActivation::Continuous(p)), true) => {
                let p = p.clone();
                let allocations = allocations.clone();
                Arc::new(move || {
                    ContinuousBackend::new_with_allocations(
                        p.api_key.clone(),
                        p.continuous.clone(),
                        allocations.clone(),
                    )
                    .map(|backend| Some(ActivatedBackend::Continuous(Arc::new(backend))))
                    .map_err(|_| "provider_configuration")
                })
            }
            (None, true) => Arc::new(|| Ok(None)),
            _ => Arc::new(|| Err("repeatable_scope_disabled")),
        };
        let (webrtc_key, warning_threshold) = match (&provider, repeatable) {
            (Some(ProviderActivation::Continuous(p)), true) => (
                Some(p.api_key.clone()),
                p.continuous.warning_threshold_microusd,
            ),
            _ => (None, 5_000_000),
        };
        let webrtc_allocations = allocations.clone();
        let provider = provider
            .map(|p| p.into_backend_with_allocations(duration, allocations))
            .transpose()?;
        let webrtc = crate::webrtc::WebRtc::new(webrtc_key, webrtc_allocations, warning_threshold)?;
        let limits = [
            crate::http::RequestLimits::default(),
            crate::http::RequestLimits::default(),
        ];
        let (slot, ticket) = Self::make_slot(
            provider,
            duration,
            max_audio_seconds,
            origins,
            &assets,
            1,
            repeatable,
            &limits,
            process_started,
        )?;
        Ok((
            Arc::new(Self {
                origins,
                assets,
                max_audio_seconds,
                repeatable,
                process_started,
                factory,
                limits,
                state: Mutex::new(State {
                    current: slot,
                    receipt: None,
                    pending: None,
                }),
                stopping: AtomicBool::new(false),
                webrtc,
            }),
            ticket,
        ))
    }
    #[allow(clippy::too_many_arguments)]
    fn make_slot(
        provider: Option<ActivatedBackend>,
        duration: Option<u64>,
        max_audio_seconds: u64,
        origins: auth::LocalOrigins,
        assets: &std::path::Path,
        generation: u64,
        repeatable: bool,
        limits: &[crate::http::RequestLimits; 2],
        process_started: Instant,
    ) -> Result<(Arc<Slot>, String), &'static str> {
        let backend = provider
            .as_ref()
            .map(ActivatedBackend::backend)
            .unwrap_or_else(engine::disabled);
        let (pilot, ticket) = Pilot::new_hosted(
            backend,
            duration.map(Duration::from_secs),
            max_audio_seconds,
            origins,
            generation,
            repeatable,
            process_started,
        )?;
        Ok((
            Arc::new(Slot {
                pause: Mutex::new(PauseStatus::active(generation)),
                pause_task: Mutex::new(None),
                operator: crate::http::router_with_limits(
                    pilot.clone(),
                    assets.to_owned(),
                    Role::Operator,
                    limits[0].clone(),
                ),
                public: crate::http::router_with_limits(
                    pilot.clone(),
                    assets.to_owned(),
                    Role::Public,
                    limits[1].clone(),
                ),
                pilot,
                initial_generation: generation,
                provider,
                #[cfg(test)]
                retirement_gate: Mutex::new(None),
            }),
            ticket,
        ))
    }
    pub(crate) fn snapshot(&self) -> Arc<Slot> {
        self.state.lock().expect("session host").current.clone()
    }
    pub fn pilot(&self) -> Arc<Pilot> {
        self.snapshot().pilot.clone()
    }

    pub(crate) fn logout(&self, pinned: &Arc<Pilot>, headers: &HeaderMap) -> Result<(), ApiError> {
        // Serialize revocation with successor publication. A logout already
        // dispatched to the predecessor also revokes its just-created receipt;
        // ordinary stale-credential requests never reach that predecessor router.
        let mut state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if let Err(error) = pinned.authorize(headers, Role::Operator, true) {
            // If the browser lost a rollover reply it still holds predecessor
            // credentials. Only logout may revoke both sides of that transition.
            let transition = state.receipt.as_ref().is_some_and(|receipt| {
                Instant::now() < receipt.deadline
                    && Arc::ptr_eq(&receipt.next.pilot, pinned)
                    && Arc::ptr_eq(&receipt.next, &state.current)
                    && receipt.authenticates_transition(headers)
            });
            if !transition {
                return Err(error);
            }
        }
        pinned.logout(Role::Operator);
        if let Some(receipt) = &state.receipt
            && Arc::ptr_eq(&receipt.next, &state.current)
        {
            if Arc::ptr_eq(&receipt.old, pinned) {
                receipt.next.pilot.logout(Role::Operator);
                state.receipt = None;
            } else if Arc::ptr_eq(&receipt.next.pilot, pinned) {
                receipt.old.logout(Role::Operator);
                state.receipt = None;
            }
        }
        Ok(())
    }

    pub(crate) async fn new_session(
        self: &Arc<Self>,
        pinned: &Arc<Pilot>,
        headers: HeaderMap,
        previous_generation: u64,
        request_id: String,
    ) -> Result<Arc<Receipt>, ApiError> {
        self.replace_session(
            pinned,
            headers,
            previous_generation,
            request_id,
            Replacement::NewSession,
        )
        .await
    }
    pub(crate) async fn resume_interpretation(
        self: &Arc<Self>,
        pinned: &Arc<Pilot>,
        headers: HeaderMap,
        previous_generation: u64,
        request_id: String,
    ) -> Result<Arc<Receipt>, ApiError> {
        self.replace_session(
            pinned,
            headers,
            previous_generation,
            request_id,
            Replacement::Resume,
        )
        .await
    }
    async fn replace_session(
        self: &Arc<Self>,
        pinned: &Arc<Pilot>,
        headers: HeaderMap,
        previous_generation: u64,
        request_id: String,
        replacement: Replacement,
    ) -> Result<Arc<Receipt>, ApiError> {
        if request_id.len() != 64
            || !request_id
                .bytes()
                .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
        {
            return Err(ApiError::Invalid);
        }
        let mut receiver = {
            let mut state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
            if self.stopping.load(Ordering::Acquire) || !self.repeatable {
                return Err(ApiError::Forbidden);
            }
            if self.webrtc.occupied() {
                return Err(ApiError::Conflict);
            }
            if let Some(receipt) = &state.receipt {
                if receipt.matches(previous_generation, &request_id, &headers, replacement) {
                    if !Arc::ptr_eq(&state.current, &receipt.next)
                        || receipt.next.pilot.is_stopped()
                    {
                        return Err(ApiError::Conflict);
                    }
                    return Ok(receipt.clone());
                }
                if Instant::now() >= receipt.deadline {
                    state.receipt = None;
                }
            }
            if let Some(pending) = &state.pending {
                if pending.replacement != replacement
                    || pending.generation != previous_generation
                    || pending.request_id != request_id
                    || pending
                        .old
                        .authorize(&headers, Role::Operator, true)
                        .is_err()
                {
                    return Err(ApiError::Conflict);
                }
                pending.receiver.clone()
            } else {
                let current = state.current.clone();
                if !Arc::ptr_eq(&current.pilot, pinned)
                    || current.initial_generation != previous_generation
                {
                    return Err(ApiError::Conflict);
                }
                let authority = current.pilot.authorize(&headers, Role::Operator, true)?;
                let eligible = match replacement {
                    Replacement::NewSession => current.pilot.stopped_generation().is_some(),
                    Replacement::Resume => {
                        current
                            .pause
                            .lock()
                            .map_err(|_| ApiError::Unavailable)?
                            .resume_allowed
                            && current.pilot.is_manual_paused()
                            && !current.pilot.is_stopped()
                    }
                };
                if !eligible {
                    return Err(ApiError::Conflict);
                }
                let next_generation = current
                    .pilot
                    .generation()
                    .checked_add(1)
                    .filter(|n| *n < (1u64 << 53) - 1)
                    .ok_or(ApiError::Capacity)?;
                let (send, receive) = watch::channel(None);
                let host = self.clone();
                let nonce = request_id.clone();
                let old = current.pilot.clone();
                let task = tokio::spawn(async move {
                    let result = host
                        .rollover(
                            current,
                            authority,
                            next_generation,
                            previous_generation,
                            nonce.clone(),
                            replacement,
                        )
                        .await;
                    {
                        let mut state = host.state.lock().expect("session host");
                        if state.pending.as_ref().is_some_and(|p| {
                            p.generation == previous_generation && p.request_id == nonce
                        }) {
                            state.pending = None;
                        }
                    }
                    send.send_replace(Some(result));
                });
                state.pending = Some(Pending {
                    generation: previous_generation,
                    replacement,
                    request_id,
                    old,
                    receiver: receive.clone(),
                    task: Some(task),
                });
                receive
            }
        };
        let result = tokio::time::timeout(Duration::from_secs(ROLLOVER_SECONDS), async {
            loop {
                if let Some(result) = receiver.borrow().clone() {
                    break result;
                }
                if receiver.changed().await.is_err() {
                    break Err(ApiError::Unavailable);
                }
            }
        })
        .await
        .map_err(|_| ApiError::Unavailable)??;
        let state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if self.stopping.load(Ordering::Acquire)
            || self.webrtc.occupied()
            || !Arc::ptr_eq(&state.current, &result.next)
            || result.next.pilot.is_stopped()
        {
            return Err(ApiError::Conflict);
        }
        Ok(result)
    }
    #[allow(clippy::too_many_arguments)]
    async fn rollover(
        &self,
        previous: Arc<Slot>,
        authority: String,
        generation: u64,
        previous_generation: u64,
        request_id: String,
        replacement: Replacement,
    ) -> Result<Arc<Receipt>, ApiError> {
        // Owned operation outlives an HTTP timeout. Cleanup is never cancelled
        // by browser navigation or a lost response, nor does it authorize audio.
        if replacement == Replacement::NewSession && !previous.retire().await {
            return Err(ApiError::Unavailable);
        }
        if self.stopping.load(Ordering::Acquire)
            || self.webrtc.occupied()
            || !previous.pilot.rollover_authority_current(&authority)
            || (replacement == Replacement::Resume && previous.pilot.is_stopped())
        {
            return Err(ApiError::Unauthorized);
        }
        let provider = (self.factory)().map_err(|_| ApiError::Unavailable)?;
        let (next, ticket) = Self::make_slot(
            provider,
            None,
            self.max_audio_seconds,
            self.origins,
            &self.assets,
            generation,
            true,
            &self.limits,
            self.process_started,
        )
        .map_err(|_| ApiError::Unavailable)?;
        let (view, token) = next.pilot.redeem(&ticket, Role::Operator)?;
        let receipt = Arc::new(Receipt {
            previous_generation,
            replacement,
            request_id,
            old: previous.pilot.clone(),
            next: next.clone(),
            view,
            token,
            deadline: Instant::now() + Duration::from_secs(RECEIPT_SECONDS),
        });
        let mut state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if self.stopping.load(Ordering::Acquire)
            || self.webrtc.occupied()
            || !Arc::ptr_eq(&state.current, &previous)
            || !previous.pilot.rollover_authority_current(&authority)
            || (replacement == Replacement::Resume && previous.pilot.is_stopped())
        {
            next.pilot.stop("process_shutdown");
            return Err(ApiError::Unauthorized);
        }
        state.current = next;
        state.receipt = Some(receipt.clone());
        Ok(receipt)
    }
    pub(crate) fn admit_webrtc(
        self: &Arc<Self>,
        pinned: &Arc<Pilot>,
        authority: String,
        headers: &HeaderMap,
        offer: crate::webrtc::Offer,
    ) -> Result<tokio::sync::oneshot::Receiver<Result<Value, ApiError>>, ApiError> {
        let state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if self.stopping.load(Ordering::Acquire)
            || !self.repeatable
            || state.pending.is_some()
            || !Arc::ptr_eq(&state.current.pilot, pinned)
        {
            return Err(ApiError::Conflict);
        }
        if pinned.authorize(headers, Role::Operator, true)? != authority {
            return Err(ApiError::Unauthorized);
        }
        self.webrtc.admit(pinned.clone(), authority, offer)
    }
    pub async fn shutdown(&self) -> bool {
        self.stopping.store(true, Ordering::Release);
        let (previous, task) = {
            let mut state = self.state.lock().expect("session host");
            let task = state
                .pending
                .as_mut()
                .and_then(|pending| pending.task.take());
            (state.current.clone(), task)
        };
        previous.pilot.stop("process_shutdown");
        let joined = match task {
            Some(task) => task.await.is_ok(),
            None => true,
        };
        let pause_task = previous.pause_task.lock().expect("pause task").take();
        let pause_joined = match pause_task {
            Some(task) => task.await.is_ok(),
            None => true,
        };
        let current = self.snapshot();
        current.pilot.stop("process_shutdown");
        let (retired, webrtc_retired) = tokio::join!(current.retire(), self.webrtc.shutdown());
        joined && pause_joined && retired && webrtc_retired
    }
}

#[cfg(test)]
mod tests;
