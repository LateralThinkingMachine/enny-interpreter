//! Rust-owned private GPT-Live WebRTC admission and sideband lifecycle. Audio
//! travels directly between the authorized browser and the provider; this owner
//! never accepts caller-selected keys, model instructions, URLs or media replay.
use crate::{
    auth::{self, ApiError, Role},
    continuous::ProcessAllocations,
    interpretation::Interpretation,
    interpretation_context::InterpretationVoice,
    runtime::Pilot,
};
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::{Arc, Mutex},
    time::{Duration, Instant},
};
use tokio::sync::{oneshot, watch};

pub(crate) mod http;
#[cfg(test)]
pub(crate) mod tests;
mod transport;

pub(crate) const DURATION_SECONDS: u64 = 300;
pub(crate) const LEASE_SECONDS: u64 = 30;
pub(crate) const MAX_SDP: usize = 64 * 1024;

#[derive(Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct Offer {
    pub generation: u64,
    pub request_id: String,
    pub sdp: String,
    pub interpretation: Interpretation,
    #[serde(default, deserialize_with = "deserialize_voice")]
    pub voice: InterpretationVoice,
    pub participant_agreement: bool,
    pub headphones: bool,
}
fn deserialize_voice<'de, D: serde::Deserializer<'de>>(
    deserializer: D,
) -> Result<InterpretationVoice, D::Error> {
    // Require the wire value to be a string. Serde's externally tagged unit
    // enum representation alone can also accept a single-key object.
    let voice = String::deserialize(deserializer)?;
    InterpretationVoice::deserialize(serde::de::value::StrDeserializer::<D::Error>::new(&voice))
}
impl Offer {
    fn validate(&self) -> Result<(), ApiError> {
        if !valid_request_id(&self.request_id)
            || !self.participant_agreement
            || !self.headphones
            || self.sdp.len() > MAX_SDP
            || !self.sdp.starts_with("v=0\r\n")
            || self.sdp.contains('\0')
            || !self.sdp.contains("m=audio ")
            || !self.sdp.contains("m=application ")
            || self.sdp.contains("m=video ")
        {
            return Err(ApiError::Invalid);
        }
        self.interpretation
            .validate(self.interpretation.anchor())
            .map_err(|_| ApiError::Invalid)
    }
    fn digest(&self) -> [u8; 32] {
        auth::hash(json!({"generation":self.generation,"sdp":self.sdp,"interpretation":self.interpretation,"voice":self.voice}).to_string().as_bytes())
    }
    fn provider_body(&self) -> Value {
        json!({"session":{"model":"gpt-live-1", "instructions":self.interpretation.instructions(),
            "audio":{"output":{"voice":self.voice}},
            "client":{"data_channel":{"allowed_client_events":["session.close"],"allowed_server_events":[
                {"type":"session.started"},{"type":"session.closed"},{"type":"session.usage.updated"},{"type":"error"},
                {"type":"session.input_transcript.delta"},{"type":"session.output_transcript.delta"}]}},"delegation":{"type":"client"},"store":false},
            "transport":{"type":"webrtc","sdp":self.sdp}})
    }
}
pub(crate) fn valid_request_id(id: &str) -> bool {
    id.len() == 64
        && id
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
}
pub(crate) fn ducking() -> Value {
    json!({"schema_version":"audio-ducking.v1","original_gain":1.0,"ducked_gain":0.20,"attack_ms":25,"release_ms":180,"hold_ms":180,"rms_on":0.008,"rms_off":0.004,"volume_ramp_ms":20,"headroom":0.5})
}
struct Observation {
    phase: &'static str,
    heartbeat: Instant,
    provider_id: Option<String>,
    answer: Option<Value>,
    closure_confirmed: bool,
    usage_seconds: Option<f64>,
    usage_confirmed: bool,
    issue: Option<&'static str>,
}
pub(crate) struct Ownership {
    request_id: String,
    digest: [u8; 32],
    generation: u64,
    voice: InterpretationVoice,
    pilot: Arc<Pilot>,
    authority: String,
    started: Instant,
    cancel: watch::Sender<bool>,
    observation: Mutex<Observation>,
    completed: watch::Sender<bool>,
}
impl Ownership {
    fn current(&self) -> bool {
        !*self.completed.borrow()
            && !*self.cancel.borrow()
            && !self.pilot.is_stopped()
            && !self.pilot.is_manual_paused()
            && self.pilot.generation() == self.generation
            && self.pilot.rollover_authority_current(&self.authority)
            && self.started.elapsed() < Duration::from_secs(DURATION_SECONDS)
            && self
                .observation
                .lock()
                .is_ok_and(|o| o.heartbeat.elapsed() < Duration::from_secs(LEASE_SECONDS))
    }
    fn stop(&self) {
        self.cancel.send_replace(true);
        if let Ok(mut o) = self.observation.lock() {
            if !o.closure_confirmed && !*self.completed.borrow() {
                o.phase = "closing";
            }
            o.answer = None;
        }
    }
    fn complete(&self, confirmed: bool, issue: Option<&'static str>) {
        if let Ok(mut o) = self.observation.lock() {
            o.phase = if confirmed {
                "closed"
            } else {
                "closure_uncertain"
            };
            o.closure_confirmed = confirmed;
            o.issue = o.issue.or(issue);
            o.answer = None;
        }
        if confirmed {
            self.pilot.release_webrtc(self.generation);
        }
        self.completed.send_replace(true);
    }
}
struct ClosedReceipt {
    authority: String,
    value: Value,
}
struct State {
    receipts: Vec<ClosedReceipt>,
    owner: Option<Arc<Ownership>>,
    consumed: BTreeSet<String>,
    cancelled: BTreeMap<String, (u64, String)>,
}
pub(crate) struct WebRtc {
    provider: Option<Arc<transport::Provider>>,
    allocations: Arc<ProcessAllocations>,
    warning_threshold: u64,
    state: Mutex<State>,
    ordinary: Arc<tokio::sync::Semaphore>,
    control: Arc<tokio::sync::Semaphore>,
}
impl WebRtc {
    pub(crate) fn new(
        key: Option<String>,
        allocations: Arc<ProcessAllocations>,
        warning_threshold: u64,
    ) -> Result<Arc<Self>, &'static str> {
        let provider = key.map(transport::Provider::new).transpose()?.map(Arc::new);
        Ok(Arc::new(Self {
            provider,
            allocations,
            warning_threshold,
            ordinary: Arc::new(tokio::sync::Semaphore::new(8)),
            control: Arc::new(tokio::sync::Semaphore::new(2)),
            state: Mutex::new(State {
                receipts: Vec::new(),
                owner: None,
                consumed: BTreeSet::new(),
                cancelled: BTreeMap::new(),
            }),
        }))
    }
    pub(crate) fn occupied(&self) -> bool {
        self.state.lock().map_or(true, |s| {
            s.owner
                .as_ref()
                .is_some_and(|o| !o.observation.lock().is_ok_and(|v| v.closure_confirmed))
        })
    }
    /// Called under SessionHost publication lock. Admission and manual route
    /// reservation share the Pilot's memory lock, so neither can race the other.
    pub(crate) fn admit(
        self: &Arc<Self>,
        pilot: Arc<Pilot>,
        authority: String,
        offer: Offer,
    ) -> Result<oneshot::Receiver<Result<Value, ApiError>>, ApiError> {
        offer.validate()?;
        let provider = self.provider.clone().ok_or(ApiError::Unavailable)?;
        let mut state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if let Some(owner) = &state.owner {
            if owner.request_id == offer.request_id
                && owner.digest == offer.digest()
                && owner.generation == offer.generation
                && Arc::ptr_eq(&owner.pilot, &pilot)
                && owner.authority == authority
            {
                let o = owner
                    .observation
                    .lock()
                    .map_err(|_| ApiError::Unavailable)?;
                let answer = o.answer.clone().filter(|_| owner.current_unlocked(&o));
                let (send, receive) = oneshot::channel();
                let _ = send.send(answer.ok_or(ApiError::Conflict));
                return Ok(receive);
            }
            if !owner.observation.lock().is_ok_and(|o| o.closure_confirmed) {
                return Err(ApiError::Conflict);
            }
        }
        if state.consumed.contains(&offer.request_id) {
            return Err(ApiError::Conflict);
        }
        if state.consumed.len() >= 128 {
            return Err(ApiError::Capacity);
        }
        pilot.claim_webrtc(offer.generation)?;
        if self
            .allocations
            .reserve_external_live(DURATION_SECONDS + 35)
            .is_err()
        {
            pilot.release_webrtc(offer.generation);
            return Err(ApiError::Capacity);
        }
        let owner = Arc::new(Ownership {
            request_id: offer.request_id.clone(),
            digest: offer.digest(),
            generation: offer.generation,
            voice: offer.voice,
            pilot,
            authority,
            started: Instant::now(),
            cancel: watch::channel(false).0,
            completed: watch::channel(false).0,
            observation: Mutex::new(Observation {
                phase: "connecting",
                heartbeat: Instant::now(),
                provider_id: None,
                answer: None,
                closure_confirmed: false,
                usage_seconds: None,
                usage_confirmed: false,
                issue: None,
            }),
        });
        if let Some(previous) = &state.owner {
            let o = previous
                .observation
                .lock()
                .map_err(|_| ApiError::Unavailable)?;
            let receipt = ClosedReceipt {
                authority: previous.authority.clone(),
                value: json!({
                "request_id":previous.request_id,"generation":previous.generation,"session_id":o.provider_id,"requested_voice":previous.voice,
                "closure_confirmed":o.closure_confirmed,"usage_confirmed":o.usage_confirmed,"usage_seconds":o.usage_seconds,"issue":o.issue }),
            };
            drop(o);
            state.receipts.push(receipt);
        }
        state.consumed.insert(offer.request_id.clone());
        state.owner = Some(owner.clone());
        let (send, receive) = oneshot::channel();
        tokio::spawn(async move {
            provider.run(owner, offer, send).await;
        });
        Ok(receive)
    }
    pub(crate) fn status(&self, pilot: &Arc<Pilot>, authority: &str) -> Value {
        let mut value = json!({"enabled":self.provider.is_some(),"state":"ready","generation":pilot.generation(),
            "closure_confirmed":true,"usage_confirmed":false,"duration_seconds":DURATION_SECONDS,"allocation_duration_seconds":DURATION_SECONDS+35,"ducking":ducking(),
            "allocated_microusd":self.allocations.snapshot().map(|v|v.0),"warning_threshold_microusd":self.warning_threshold});
        if let Ok(state) = self.state.lock() {
            value["closure_receipts"] = json!(
                state
                    .receipts
                    .iter()
                    .filter(|r| r.authority == authority)
                    .map(|r| r.value.clone())
                    .collect::<Vec<_>>()
            );
        }
        if let Ok(state) = self.state.lock()
            && let Some(owner) = &state.owner
        {
            if Arc::ptr_eq(&owner.pilot, pilot) && owner.authority == authority {
                if let Ok(o) = owner.observation.lock() {
                    value["state"] = json!(o.phase);
                    value["generation"] = json!(owner.generation);
                    value["request_id"] = json!(owner.request_id);
                    value["session_id"] = json!(o.provider_id);
                    value["requested_voice"] = json!(owner.voice);
                    value["closure_confirmed"] = json!(o.closure_confirmed);
                    value["usage_confirmed"] = json!(o.usage_confirmed);
                    value["usage_seconds"] = json!(o.usage_seconds);
                    value["issue"] = json!(o.issue);
                }
            } else if !owner.observation.lock().is_ok_and(|o| o.closure_confirmed) {
                value["state"] = json!("busy");
                value["closure_confirmed"] = json!(false);
            }
        }
        value
    }
    pub(crate) fn control(
        &self,
        pilot: &Arc<Pilot>,
        authority: &str,
        generation: u64,
        request_id: &str,
        stop: bool,
    ) -> Result<(), ApiError> {
        let mut state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        let owner = state.owner.as_ref().filter(|o| {
            Arc::ptr_eq(&o.pilot, pilot)
                && o.authority == authority
                && o.generation == generation
                && o.request_id == request_id
        });
        let Some(owner) = owner else {
            // A browser can stop while its offer body is still arriving. Fence
            // that exact pending admission under the same lock as admit, even
            // though no provider owner exists yet. Never let stale Stop revoke
            // a successor, and never infer authority from its submitted ID.
            if !stop
                || pilot.generation() != generation
                || !pilot.rollover_authority_current(authority)
            {
                return Err(ApiError::Conflict);
            }
            if state
                .cancelled
                .get(request_id)
                .is_some_and(|(g, a)| *g == generation && a == authority)
            {
                return Ok(());
            }
            if state.consumed.contains(request_id) {
                return Err(ApiError::Conflict);
            }
            if state.consumed.len() >= 128 {
                return Err(ApiError::Capacity);
            }
            state.consumed.insert(request_id.to_owned());
            state
                .cancelled
                .insert(request_id.to_owned(), (generation, authority.to_owned()));
            return Ok(());
        };
        if stop {
            owner.stop();
        } else {
            if !owner.current() {
                return Err(ApiError::Stopped);
            }
            owner
                .observation
                .lock()
                .map_err(|_| ApiError::Unavailable)?
                .heartbeat = Instant::now();
        }
        Ok(())
    }
    pub(crate) fn answer_current(
        &self,
        pilot: &Arc<Pilot>,
        authority: &str,
        generation: u64,
        request_id: &str,
    ) -> bool {
        self.state.lock().is_ok_and(|state| {
            state.owner.as_ref().is_some_and(|owner| {
                Arc::ptr_eq(&owner.pilot, pilot)
                    && owner.authority == authority
                    && owner.generation == generation
                    && owner.request_id == request_id
                    && owner.current()
            })
        })
    }
    pub(crate) fn cancellation_receipt(
        &self,
        authority: &str,
        generation: u64,
        request_id: &str,
    ) -> Option<Value> {
        let state = self.state.lock().ok()?;
        state.cancelled.get(request_id)
            .filter(|(g, a)| *g == generation && a == authority)
            .map(|_| json!({
                "enabled":self.provider.is_some(),"state":"closed","generation":generation,
                "request_id":request_id,"session_id":null,"closure_confirmed":true,
                "closure_basis":"cancelled_before_admission","usage_confirmed":false,"usage_seconds":null,
                "issue":"start_cancelled","duration_seconds":DURATION_SECONDS,
                "allocation_duration_seconds":DURATION_SECONDS+35,"ducking":ducking(),
                "allocated_microusd":self.allocations.snapshot().map(|v|v.0),
                "warning_threshold_microusd":self.warning_threshold
            }))
    }
    pub(crate) async fn shutdown(&self) -> bool {
        let owner = self.state.lock().ok().and_then(|s| s.owner.clone());
        let Some(owner) = owner else {
            return true;
        };
        owner.stop();
        let mut done = owner.completed.subscribe();
        let _ = tokio::time::timeout(Duration::from_secs(31), async {
            while !*done.borrow() {
                if done.changed().await.is_err() {
                    break;
                }
            }
        })
        .await;
        owner.observation.lock().is_ok_and(|o| o.closure_confirmed)
    }
}
impl Ownership {
    fn current_unlocked(&self, o: &Observation) -> bool {
        !*self.completed.borrow()
            && !*self.cancel.borrow()
            && !self.pilot.is_stopped()
            && !self.pilot.is_manual_paused()
            && self.pilot.generation() == self.generation
            && self.pilot.rollover_authority_current(&self.authority)
            && self.started.elapsed() < Duration::from_secs(DURATION_SECONDS)
            && o.heartbeat.elapsed() < Duration::from_secs(LEASE_SECONDS)
    }
}
