//! Source-only termination barrier. Capture admission and the input FIFO share
//! the memory lock, so FinishInput follows every accepted PCM frame. This is
//! not sticky Stop: only existing private ASR observation remains authorized.
use super::*;
use std::collections::BTreeSet;

pub(super) struct FinishState {
    pending: BTreeSet<String>,
    pub(super) incomplete: bool,
    no_active_asr: bool,
    deadline: Instant,
    emitted: bool,
}

impl Pilot {
    /// Single use for this process generation. The HTTP acknowledgement is not
    /// completion: the existing private socket must receive `finished` in order.
    pub fn finish(self: &Arc<Self>, generation: u64) -> Result<(), ApiError> {
        self.begin_finish(generation, Duration::from_secs(30), None)
    }

    /// Called only for an ordered control message on the existing authenticated
    /// operator socket. The exact subscription/session is rechecked under lock.
    pub fn finish_from_socket(
        self: &Arc<Self>,
        generation: u64,
        id: &str,
        session: &str,
    ) -> Result<(), ApiError> {
        self.begin_finish(generation, Duration::from_secs(30), Some((id, session)))
    }

    fn begin_finish(
        self: &Arc<Self>,
        generation: u64,
        limit: Duration,
        socket: Option<(&str, &str)>,
    ) -> Result<(), ApiError> {
        self.observe(generation)?;
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.is_manual_paused() {
            return Err(ApiError::Stopped);
        }
        if memory.finish.is_some() {
            return Err(ApiError::Conflict);
        }
        if memory
            .operator_connection
            .as_ref()
            .is_none_or(|c| c.heartbeat.elapsed() >= self.heartbeat_lease(Role::Operator))
        {
            return Err(ApiError::Unauthorized);
        }
        if let Some((id, session)) = socket
            && (memory
                .operator_connection
                .as_ref()
                .is_none_or(|c| c.id != id || c.session != session)
                || memory.operator.as_ref().is_none_or(|s| s.id != session))
        {
            return Err(ApiError::Unauthorized);
        }
        // All positive operations recheck this fence while holding this lock.
        self.finishing.store(true, Ordering::Release);
        // Fence factory admission synchronously, before awaiting queue space
        // for the ordered source FinishInput barrier. Existing source output
        // may drain, but no continuous route may purchase its next window.
        for route in memory.routes.values() {
            if let Some(fence) = &route.renewal_fence {
                fence.send_replace(true);
            }
        }
        let deadline = self.operation_deadline(limit);
        let asr_count = memory
            .routes
            .values()
            .filter(|r| r.route == Route::Transcription)
            .count();
        let selected: Vec<_> = memory
            .routes
            .iter()
            .filter_map(|(id, r)| {
                (r.route == Route::Transcription && r.ready)
                    .then(|| r.input.clone().map(|input| (id.clone(), input)))
                    .flatten()
            })
            .collect();
        let pending: BTreeSet<_> = selected.iter().map(|(id, _)| id.clone()).collect();
        memory.finish = Some(FinishState {
            incomplete: memory.sources.iter().any(|s| s.closed || s.interrupted)
                || pending.len() != asr_count
                || (socket.is_none() && asr_count > 0),
            no_active_asr: asr_count == 0,
            pending: pending.clone(),
            deadline,
            emitted: false,
        });
        // Revoke public transport/read authority and prepared capabilities now.
        memory.public_ticket = None;
        memory.viewer = None;
        memory.public_connection = None;
        memory.delivery = None;
        memory.previews.clear();
        self.public_changed.send_modify(|e| *e += 1);
        let removed: Vec<_> = memory
            .routes
            .keys()
            .filter(|id| !pending.contains(*id))
            .cloned()
            .collect();
        // This acknowledgement is ordered on the same private egress FIFO as
        // final source items and the completion marker, including a zero-route finish.
        let mut failed =
            !Self::queue_private(&memory, json!({"type":"finishing","generation":generation}));
        for id in removed {
            let route = memory.routes.remove(&id).expect("selected existing route");
            route.cancel.send_replace(true);
            if let Some(stop) = &route.stop {
                stop.send_replace(true);
            }
            failed |= !Self::queue_private(&memory, route_closed(generation, &id, &route));
        }
        self.finish_changed.send_modify(|e| *e += 1);
        drop(memory);
        if failed {
            self.stop("backpressure");
            return Err(ApiError::Stopped);
        }
        for (id, input) in selected {
            let pilot = Arc::clone(self);
            tokio::spawn(async move {
                pilot
                    .send_finish_input(generation, id, input, deadline)
                    .await;
            });
        }
        let pilot = Arc::clone(self);
        tokio::spawn(async move {
            pilot.finish_barrier(generation, deadline).await;
        });
        Ok(())
    }

    async fn send_finish_input(
        self: Arc<Self>,
        generation: u64,
        id: String,
        input: mpsc::Sender<StreamInput>,
        deadline: Instant,
    ) {
        let mut stop = self.stop_receiver();
        let send = input.send(StreamInput::FinishInput);
        tokio::pin!(send);
        let checked = std::future::poll_fn(|cx| {
            let mut memory = self.memory.lock().expect("pilot state");
            if self.is_stopped()
                || self.generation() != generation
                || Instant::now() >= deadline
                || !memory.routes.contains_key(&id)
            {
                Poll::Ready(false)
            } else {
                send.as_mut().poll(cx).map(|r| {
                    if r.is_ok() {
                        memory
                            .routes
                            .get_mut(&id)
                            .expect("checked route")
                            .finish_sent = true;
                    }
                    r.is_ok()
                })
            }
        });
        let sent = tokio::select! { biased;
            _=stop.changed()=>false,
            _=tokio::time::sleep_until(deadline.into())=>false,
            sent=checked=>sent,
        };
        if !sent {
            let _ = self.close_route(&id);
        }
    }

    async fn finish_barrier(self: Arc<Self>, generation: u64, deadline: Instant) {
        let mut stop = self.stop_receiver();
        let mut changed = self.finish_changed.subscribe();
        loop {
            if self.observe(generation).is_err() {
                return;
            }
            let pending = {
                let memory = self.memory.lock().expect("pilot state");
                let Some(finish) = &memory.finish else {
                    return;
                };
                if finish.emitted {
                    return;
                }
                finish.pending.iter().cloned().collect::<Vec<_>>()
            };
            if pending.is_empty() {
                break;
            }
            if Instant::now() >= deadline {
                for id in pending {
                    let _ = self.close_route(&id);
                }
                break;
            }
            tokio::select! { biased;
                _=stop.changed()=>return,
                _=tokio::time::sleep_until(deadline.into())=>{},
                _=changed.changed()=>{},
            }
        }
        let mut memory = self.memory.lock().expect("pilot state");
        if self.is_stopped() || self.generation() != generation {
            return;
        }
        let Some(finish) = memory.finish.as_mut() else {
            return;
        };
        if finish.emitted {
            return;
        }
        let completion = if finish.incomplete {
            "incomplete"
        } else if finish.no_active_asr {
            "no_active_asr"
        } else {
            "complete"
        };
        finish.emitted = true;
        let failed = !Self::queue_private(
            &memory,
            json!({"type":"finished","generation":generation,"source_completion":completion}),
        );
        drop(memory);
        if failed {
            self.stop("backpressure");
        }
    }

    pub(super) fn finish_selected(memory: &Memory, id: &str) -> bool {
        memory
            .finish
            .as_ref()
            .is_some_and(|f| !f.emitted && f.pending.contains(id) && Instant::now() < f.deadline)
    }

    pub(super) fn retire_finish_route(&self, memory: &mut Memory, id: &str, complete: bool) {
        if let Some(finish) = &mut memory.finish
            && finish.pending.remove(id)
        {
            finish.incomplete |= !complete || Instant::now() >= finish.deadline;
            self.finish_changed.send_modify(|e| *e += 1);
        }
    }

    pub(super) fn queue_private(memory: &Memory, event: Value) -> bool {
        memory
            .operator_connection
            .as_ref()
            .is_some_and(|c| c.events.try_send(event).is_ok())
    }

    pub(super) fn emit_route_event(&self, id: &str, mut event: Value) {
        let memory = self.memory.lock().expect("pilot state");
        if self.is_stopped()
            || self.is_manual_paused()
            || !memory.routes.contains_key(id)
            || (memory.finish.is_some() && !Self::finish_selected(&memory, id))
        {
            return;
        }
        if matches!(
            event["type"].as_str(),
            Some("route" | "audio" | "live_transcript" | "transcript")
        ) && let Some(policy) = memory.routes.get(id).and_then(|r| r.interpretation)
        {
            event["interpretation"] = json!(policy);
        }
        let failed = !Self::queue_private(&memory, event);
        drop(memory);
        if failed {
            self.stop("backpressure");
        }
    }

    pub(super) fn retire_route(&self, id: &str, complete: bool) -> Result<(), ApiError> {
        let mut memory = self.memory.lock().expect("pilot state");
        let route = memory.routes.remove(id).ok_or(ApiError::Invalid)?;
        route.cancel.send_replace(true);
        if let Some(stop) = &route.stop {
            stop.send_replace(true);
        }
        let failed = !Self::queue_private(&memory, route_closed(self.generation(), id, &route));
        // Terminal queue entry precedes barrier readiness under the same lock.
        self.retire_finish_route(&mut memory, id, complete);
        drop(memory);
        if failed {
            self.stop("backpressure");
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;

fn route_closed(generation: u64, id: &str, route: &ActiveRoute) -> Value {
    let mut event = json!({"type":"route","generation":generation,"route_id":id,"source":route.source,"route":route.route,"language":route.language,"live_mode":route.live_mode,"state":"closed","code":null,"expires_in_seconds":null});
    if let Some(policy) = route.interpretation {
        event["interpretation"] = json!(policy);
    }
    event
}
