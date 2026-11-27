//! Manual spending pause is a one-way fence on the current Pilot. Resume
//! replaces it through SessionHost and never resets a ledger or unlatches Stop.
use super::*;
use serde::Serialize;

#[derive(Clone, Copy, Serialize)]
pub(crate) struct PauseStatus {
    schema_version: &'static str,
    generation: u64,
    state: &'static str,
    pub(super) local_retired: bool,
    pub(super) provider_closed_confirmed: bool,
    pub(super) resume_allowed: bool,
}
impl PauseStatus {
    pub(super) fn active(generation: u64) -> Self {
        Self {
            schema_version: "interpretation-lifecycle.v1",
            generation,
            state: "active",
            local_retired: false,
            provider_closed_confirmed: false,
            resume_allowed: false,
        }
    }
}
impl SessionHost {
    pub(crate) fn interpretation_state(
        &self,
        pinned: &Arc<Pilot>,
        headers: &HeaderMap,
    ) -> Result<PauseStatus, ApiError> {
        pinned.authorize(headers, Role::Operator, false)?;
        let state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if !Arc::ptr_eq(&state.current.pilot, pinned) || !self.repeatable {
            return Err(ApiError::Forbidden);
        }
        let mut status = *state
            .current
            .pause
            .lock()
            .map_err(|_| ApiError::Unavailable)?;
        if self.stopping.load(Ordering::Acquire) || pinned.is_stopped() {
            status.resume_allowed = false;
        }
        Ok(status)
    }
    pub(crate) fn pause_interpretation(
        self: &Arc<Self>,
        pinned: &Arc<Pilot>,
        headers: &HeaderMap,
        generation: u64,
    ) -> Result<PauseStatus, ApiError> {
        pinned.authorize(headers, Role::Operator, true)?;
        let state = self.state.lock().map_err(|_| ApiError::Unavailable)?;
        if !Arc::ptr_eq(&state.current.pilot, pinned)
            || self.stopping.load(Ordering::Acquire)
            || !self.repeatable
            || self.webrtc.occupied()
        {
            return Err(ApiError::Forbidden);
        }
        let slot = state.current.clone();
        Self::pause_slot(&slot, generation)
    }

    fn pause_slot(slot: &Arc<Slot>, generation: u64) -> Result<PauseStatus, ApiError> {
        let mut status = slot.pause.lock().map_err(|_| ApiError::Unavailable)?;
        // Revalidate generation and sticky Stop even for an idempotent retry.
        slot.pilot.pause_processing(generation)?;
        if status.state != "active" {
            return Ok(*status);
        }
        status.state = "pausing";
        let retiring = slot.clone();
        let task = tokio::spawn(async move {
            let retired = tokio::time::timeout(Duration::from_secs(16), retiring.retire())
                .await
                .unwrap_or(false);
            retiring.pilot.mark_pause_retired(retired);
            let confirmed = retired
                && retiring
                    .provider
                    .as_ref()
                    .is_none_or(ActivatedBackend::closure_confirmed);
            let mut status = retiring.pause.lock().expect("pause status");
            status.local_retired = retired;
            status.provider_closed_confirmed = confirmed;
            status.state = if confirmed {
                "paused"
            } else {
                "closure_unconfirmed"
            };
            status.resume_allowed = confirmed && !retiring.pilot.is_stopped();
        });
        *slot.pause_task.lock().map_err(|_| ApiError::Unavailable)? = Some(task);
        Ok(*status)
    }
}
