use super::{
    Reservation, Shared, audio,
    diagnostics::{
        DiagnosticCategory as Category, DiagnosticEvent as Event, DiagnosticStage as Stage,
        ProviderDiagnostic, error_category,
    },
    guard,
    protocol::{self, Protocol},
};
use crate::engine::{
    DrainCompletion, Language, LiveMode, PreparedAudio, Provenance, ProviderError, Route,
    StreamEvent, StreamHandle, StreamInput,
};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    future::{Future, poll_fn},
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
};
use tokio_tungstenite::{
    Connector, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Message, client::IntoClientRequest, http::HeaderValue, protocol::WebSocketConfig,
    },
};

fn boundary_error(error: super::local_vad::BoundaryError) -> ProviderError {
    match error {
        super::local_vad::BoundaryError::TurnLimit => ProviderError::SpeechBoundaryLimit,
        super::local_vad::BoundaryError::IncompleteCandidate => ProviderError::SourceIncomplete,
        _ => ProviderError::Protocol,
    }
}

/// A parsed final can complete the ledger just as its publication deadline
/// passes. Such an event is deliberately not delivered by the incoming path;
/// ledger completion must not then turn that cutoff into a successful drain.
pub(super) fn finishing_completion(
    now: Instant,
    end: Instant,
    committed: bool,
) -> Option<DrainCompletion> {
    if now >= end {
        Some(DrainCompletion::ObservationWindowEnded)
    } else if committed {
        Some(DrainCompletion::CommittedTranscriptsFinal)
    } else {
        None
    }
}

/// Timers are wakeups, not authorization. Recheck the monotonic deadline before
/// *each* inner poll, even if the transport is immediately ready or timer stalls.
pub(super) async fn deadline<F: Future>(
    at: Instant,
    future: F,
) -> Result<F::Output, ProviderError> {
    tokio::pin!(future);
    let checked = poll_fn(|cx| {
        if Instant::now() >= at {
            Poll::Ready(Err(ProviderError::Deadline))
        } else {
            future.as_mut().poll(cx).map(Ok)
        }
    });
    tokio::select! {biased;_=tokio::time::sleep_until(at.into())=>Err(ProviderError::Deadline),result=checked=>result}
}
#[cfg(test)]
pub(super) async fn active<F: Future>(
    at: Instant,
    local: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    future: F,
) -> Result<F::Output, ProviderError> {
    let external = guard::ExternalRevocation::new(Vec::new())?;
    guard::active(at, local, global, &external, future).await
}
pub(super) fn tls() -> Result<rustls::ClientConfig, ProviderError> {
    let roots = rustls::RootCertStore::from_iter(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
    rustls::ClientConfig::builder_with_provider(Arc::new(rustls::crypto::ring::default_provider()))
        .with_safe_default_protocol_versions()
        .map_err(|_| ProviderError::Unavailable)
        .map(|b| b.with_root_certificates(roots).with_no_client_auth())
}
fn http() -> Result<reqwest::Client, ProviderError> {
    reqwest::Client::builder()
        .use_preconfigured_tls(tls()?)
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .https_only(true)
        .connect_timeout(Duration::from_secs(3))
        .timeout(Duration::from_secs(65))
        .build()
        .map_err(|_| ProviderError::Unavailable)
}
fn auth(key: &str) -> Result<HeaderValue, ProviderError> {
    let mut h =
        HeaderValue::from_str(&format!("Bearer {key}")).map_err(|_| ProviderError::Protocol)?;
    h.set_sensitive(true);
    Ok(h)
}
pub(super) fn ws_config() -> WebSocketConfig {
    WebSocketConfig::default()
        .write_buffer_size(0)
        .max_write_buffer_size(131_072)
        .max_message_size(Some(65_536))
        .max_frame_size(Some(65_536))
}

pub(super) fn handshake_status(error: &tokio_tungstenite::tungstenite::Error) -> Option<u16> {
    // Never format the error: Http contains upstream headers and response body.
    match error {
        tokio_tungstenite::tungstenite::Error::Http(response) => Some(response.status().as_u16()),
        _ => None,
    }
}

/// Never retry an auth/TLS/schema/size rejection as if it were a network outage.
pub(super) fn transport_error(error: &tokio_tungstenite::tungstenite::Error) -> ProviderError {
    use tokio_tungstenite::tungstenite::{Error, error::ProtocolError};
    match error {
        Error::Io(error)
            if matches!(
                error.kind(),
                std::io::ErrorKind::ConnectionReset
                    | std::io::ErrorKind::ConnectionRefused
                    | std::io::ErrorKind::ConnectionAborted
                    | std::io::ErrorKind::NotConnected
                    | std::io::ErrorKind::BrokenPipe
                    | std::io::ErrorKind::TimedOut
                    | std::io::ErrorKind::UnexpectedEof
                    | std::io::ErrorKind::NetworkUnreachable
                    | std::io::ErrorKind::HostUnreachable
            ) =>
        {
            ProviderError::ConnectionLost
        }
        Error::ConnectionClosed
        | Error::AlreadyClosed
        | Error::Protocol(ProtocolError::ResetWithoutClosingHandshake) => {
            ProviderError::ConnectionLost
        }
        Error::Http(response) if response.status().is_server_error() => {
            ProviderError::ConnectionLost
        }
        _ => ProviderError::Unavailable,
    }
}

pub(super) async fn open(
    shared: Arc<Shared>,
    route: Route,
    language: Language,
    mode: Option<LiveMode>,
) -> Result<StreamHandle, ProviderError> {
    open_configured(shared, route, language, mode, None).await
}
pub(super) async fn open_configured(
    shared: Arc<Shared>,
    route: Route,
    language: Language,
    mode: Option<LiveMode>,
    hints: Option<crate::engine::SpeechHints>,
) -> Result<StreamHandle, ProviderError> {
    open_extended(shared, route, language, mode, hints, None, None).await
}
pub(super) async fn open_extended(
    shared: Arc<Shared>,
    route: Route,
    language: Language,
    mode: Option<LiveMode>,
    hints: Option<crate::engine::SpeechHints>,
    interpretation: Option<crate::engine::Interpretation>,
    context: Option<crate::interpretation_context::InterpretationContext>,
) -> Result<StreamHandle, ProviderError> {
    let mut protocol = Protocol::with_mode(route, language, mode)?;
    if let Some(policy) = interpretation {
        protocol.configure_interpretation(policy)?;
    }
    if let Some(context) = context {
        protocol.configure_interpretation_context(context)?;
    }
    let expanded_source = route == Route::Transcription
        && hints
            .as_ref()
            .is_some_and(|hints| hints.languages.contains(&language));
    if !language.legacy() && interpretation.is_none() && !expanded_source {
        return Err(ProviderError::Protocol);
    }
    if (!language.legacy()
        || interpretation.is_some()
        || hints
            .as_ref()
            .is_some_and(|h| h.languages.iter().any(|l| !l.legacy())))
        && !shared.multilingual
    {
        return Err(ProviderError::Unavailable);
    }
    if let Some(hints) = hints {
        protocol.configure_transcription(hints)?;
    }
    if protocol
        .live_mode
        .is_some_and(|mode| !shared.config.allowed_live_modes.contains(&mode))
    {
        return Err(ProviderError::Unavailable);
    }
    let reservation = shared.reserve(Some(route), 0)?;
    let (input, input_rx) = mpsc::channel(8);
    let (events, event_tx) = mpsc::channel(32);
    let (stop, stop_rx) = watch::channel(false);
    let task_shared = shared.clone();
    shared.spawn_owned(async move {
        run(
            task_shared,
            reservation,
            protocol,
            input_rx,
            events,
            stop_rx,
        )
        .await;
    })?;
    Ok(StreamHandle {
        input,
        events: event_tx,
        stop,
        renewal_fence: None,
    })
}
async fn run(
    shared: Arc<Shared>,
    mut reservation: Reservation,
    mut p: Protocol,
    input: mpsc::Receiver<StreamInput>,
    events: mpsc::Sender<StreamEvent>,
    mut stop: watch::Receiver<bool>,
) {
    let started = Instant::now();
    let hard = reservation.stream_deadline(started);
    let mut global = shared.stop.subscribe();
    let route = p.route;
    p.max_seconds = reservation.seconds;
    let mut http_status = None;
    let connected = async {
        let mut request = protocol::endpoint(route)
            .into_client_request()
            .map_err(|_| ProviderError::Unavailable)?;
        request
            .headers_mut()
            .insert("Authorization", auth(&shared.config.api_key)?);
        connect_async_tls_with_config(
            request,
            Some(ws_config()),
            true,
            Some(Connector::Rustls(Arc::new(tls()?))),
        )
        .await
        .map_err(|error| {
            http_status = handshake_status(&error);
            transport_error(&error)
        })
    };
    let result = guard::until_admitted(
        (started + Duration::from_secs(5)).min(hard),
        &mut stop,
        &mut global,
        &shared.revocation,
        Some(&shared.admission),
        connected,
    )
    .await;
    let mut ws = match result {
        Ok(Ok((ws, _))) => ws,
        failure => {
            let category = if http_status.is_some() {
                Category::HttpRefused
            } else if matches!(failure, Err(ProviderError::Deadline)) {
                Category::Deadline
            } else {
                Category::Transport
            };
            shared.record_diagnostic(ProviderDiagnostic {
                route,
                stage: Stage::Handshake,
                category,
                event_type: Event::None,
                upstream_code: Default::default(),
                upstream_parameter: Default::default(),
                http_status,
                ready: false,
                closed_confirmed: false,
            });
            let error = match failure {
                Ok(Err(error)) => error,
                Err(ProviderError::Deadline) => ProviderError::ConnectionLost,
                _ => ProviderError::Unavailable,
            };
            let _ = events.try_send(StreamEvent::Error(error));
            let _ = events.try_send(StreamEvent::Closed { confirmed: false });
            return;
        }
    };
    let report = drive(
        &shared,
        &mut ws,
        &mut p,
        input,
        &events,
        &mut stop,
        &mut global,
        hard,
    )
    .await;
    let dirty = report.as_ref().is_err_and(|(dirty, _)| *dirty);
    let initial_failure = report.as_ref().err().map(|(_, error)| {
        let (event, category) = p.failure.unwrap_or((Event::None, error_category(*error)));
        (
            if p.ready {
                Stage::Streaming
            } else {
                Stage::Startup
            },
            event,
            category,
        )
    });
    publish_failure(&events, &report);
    let cleanup = (Instant::now() + Duration::from_secs(10)).min(hard);
    let confirmed = if dirty {
        false
    } else {
        close(&mut ws, &mut p, cleanup).await
    };
    // Never flush a transport whose application write was cancelled/partial.
    drop(ws);
    if !confirmed
        && route == Route::Live
        && let Some(id) = p.id.as_deref()
    {
        let _ = hangup(&shared, id, cleanup).await;
    }
    let receipt_complete = route != Route::Transcription || p.complete_input();
    // A peer close can prove resource retirement without proving final usage
    // for intentionally uncommitted/unfinished audio. Never reopen work from
    // that uncertain receipt; the entire reservation remains spent.
    reservation.completed = confirmed && receipt_complete;
    if !reservation.completed {
        shared.poison();
    }
    if confirmed && !receipt_complete {
        let _ = events.try_send(StreamEvent::Error(ProviderError::SourceIncomplete));
    }
    let (stage, event_type, category) = initial_failure.unwrap_or({
        if let Some((event, category)) = p.failure {
            (Stage::Closing, event, category)
        } else if confirmed && !receipt_complete {
            (Stage::Closing, Event::None, Category::SourceIncomplete)
        } else if confirmed {
            (Stage::Complete, Event::None, Category::Confirmed)
        } else {
            (Stage::Closing, Event::None, Category::CloseUnconfirmed)
        }
    });
    shared.record_diagnostic(ProviderDiagnostic {
        route,
        stage,
        category,
        event_type,
        upstream_code: p.failure_details.0,
        upstream_parameter: p.failure_details.1,
        http_status: None,
        ready: p.ready,
        closed_confirmed: confirmed,
    });
    let _ = events.try_send(StreamEvent::Closed { confirmed });
}

/// A failure remains observable after shared authority is revoked. This is
/// synchronous and precedes closure: never suppress the cause because its
/// resource ceiling already stopped the paired stream. A full consumer queue
/// still fails closed at the runtime boundary; this does not promise delivery.
pub(super) fn publish_failure(
    events: &mpsc::Sender<StreamEvent>,
    report: &Result<(), (bool, ProviderError)>,
) {
    if let Err((_, error)) = report {
        let _ = events.try_send(StreamEvent::Error(*error));
    }
}
#[allow(clippy::too_many_arguments)]
pub(super) async fn drive<S: AsyncRead + AsyncWrite + Unpin>(
    shared: &Shared,
    ws: &mut WebSocketStream<S>,
    p: &mut Protocol,
    mut input: mpsc::Receiver<StreamInput>,
    events: &mpsc::Sender<StreamEvent>,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    hard: Instant,
) -> Result<(), (bool, ProviderError)> {
    let active_end = hard - Duration::from_secs(10);
    let mut ready_end = (Instant::now() + Duration::from_secs(5)).min(active_end);
    // Protocol readiness is not admission until its Ready event is released.
    // Keep this per stream so one ready stream cannot admit a sibling startup.
    let mut admitted = false;
    let mut configured = p.route == Route::Live;
    if configured {
        write_guarded(
            shared,
            ws,
            p.startup(),
            ready_end,
            stop,
            global,
            Some(&shared.admission),
        )
        .await
        .map_err(|e| (true, e))?;
    }
    let mut converter = audio::Upsample::new();
    let mut local_vad = if p.route == Route::Transcription {
        Some(super::local_vad::LocalVad::new().map_err(|_| (false, ProviderError::Protocol))?)
    } else {
        None
    };
    let mut selected_end = None;
    let mut last_input = Instant::now();
    let mut received_audio = false;
    let mut input_origin = None;
    let mut finishing: Option<(Instant, Instant)> = None;
    let mut boundary_handover = false;
    let mut sealed_discarded = 0u8;
    let mut last_output = Instant::now();
    let mut next_clock: Option<Instant> = None;
    let mut prefer_live_input = false;
    'drive: loop {
        let authority = p
            .expiry
            .map(|e| e - Duration::from_secs(10))
            .unwrap_or(active_end)
            .min(active_end);
        if *stop.borrow()
            || *global.borrow()
            || shared.revocation.revoked()
            || (!admitted && shared.admission.revoked())
            || Instant::now() >= authority
        {
            if let Some(vad) = local_vad.as_mut() {
                vad.stop();
            }
            if !*stop.borrow()
                && !*global.borrow()
                && !shared.revocation.revoked()
                && (admitted || !shared.admission.revoked())
                && finishing.is_some()
            {
                events
                    .try_send(StreamEvent::Drained {
                        completion: DrainCompletion::ObservationWindowEnded,
                    })
                    .map_err(|_| (false, ProviderError::ProtocolCapacityLimit))?;
            }
            return Ok(());
        }
        if p.route == Route::Transcription {
            p.asr.check(p.frames).map_err(|e| (false, e))?;
        }
        if let Some((since, end)) = finishing {
            let committed = p.route == Route::Transcription
                && local_vad.as_ref().is_none_or(|vad| !vad.active())
                && p.complete_input()
                // Manual boundaries already account for every selected frame,
                // receipt, final and unbound draft. Once sealed, that ledger is
                // the completion witness; an extra quiet second cannot add
                // certainty and lengthens every continuous handover gap.
                // Retained legacy server-VAD traces keep their settling guard.
                && (p.asr.is_local()
                    || (since.elapsed() >= Duration::from_secs(1)
                        && last_output.elapsed() >= Duration::from_secs(1)));
            if let Some(completion) = finishing_completion(Instant::now(), end, committed) {
                events
                    .try_send(StreamEvent::Drained { completion })
                    .map_err(|_| (false, ProviderError::ProtocolCapacityLimit))?;
                return Ok(());
            }
        }
        if p.ready {
            ready_end = authority;
        }
        enum Wake {
            Input(Option<StreamInput>),
            Incoming(Option<Result<Message, tokio_tungstenite::tungstenite::Error>>),
            Stop,
            Timeout,
            InputInterrupted,
            DrainCheck,
            Clock,
        }
        let mut guarded_stop = stop.clone();
        let mut guarded_global = global.clone();
        // Live is full duplex. A ready output burst must not starve queued
        // microphone frames, nor may microphone frames starve model output.
        // Other routes retain incoming receipt priority for their existing
        // source/final ordering. Stop and all clocks remain in the outer gate.
        let media = async {
            if p.route == Route::Live && prefer_live_input {
                tokio::select! {biased;
                    value=input.recv(),if p.ready=>Wake::Input(value),
                    value=ws.next()=>Wake::Incoming(value),
                }
            } else {
                tokio::select! {biased;
                    value=ws.next()=>Wake::Incoming(value),
                    value=input.recv(),if p.ready=>Wake::Input(value),
                }
            }
        };
        let wake = tokio::select! {biased;
         _=stop.changed()=>Wake::Stop,_=global.changed()=>Wake::Stop,
         _=tokio::time::sleep_until(ready_end.into())=>Wake::Timeout,
         _=tokio::time::sleep_until((last_input+shared.input_idle_grace).into()),if received_audio && finishing.is_none()=>Wake::InputInterrupted,
         _=tokio::time::sleep_until(p.asr.deadline().unwrap_or(authority).into()),if p.route==Route::Transcription=>Wake::Timeout,
         _=tokio::time::sleep(Duration::from_millis(100)),if finishing.is_some()=>Wake::DrainCheck,
         _=tokio::time::sleep_until(next_clock.unwrap_or(authority).into()),if next_clock.is_some()=>Wake::Clock,
         v=guard::until_admitted(authority, &mut guarded_stop, &mut guarded_global, &shared.revocation, (!admitted).then_some(&shared.admission), media)=>match v {
            Ok(value)=>value,
            Err(_) if *stop.borrow() || *global.borrow() || shared.revocation.revoked() || (!admitted && shared.admission.revoked())=>Wake::Stop,
            Err(_)=>Wake::Timeout,
         },
        };
        match &wake {
            Wake::Incoming(_) => prefer_live_input = true,
            Wake::Input(_) => prefer_live_input = false,
            _ => {}
        }
        match wake {
            Wake::Stop => {
                if let Some(vad) = local_vad.as_mut() {
                    vad.stop();
                }
                return Ok(());
            }
            Wake::DrainCheck => continue,
            Wake::Clock => {
                let (_, end) = finishing.ok_or((false, ProviderError::Protocol))?;
                if p.route != Route::Live || p.close_requested {
                    return Err((false, ProviderError::Protocol));
                }
                if Instant::now() >= end {
                    continue;
                }
                // Live needs a continuous input stream while speaking. These
                // local zeros advance model time, never source capture ordinals.
                // They spend the same finite input allowance as captured PCM.
                shared.input(320).map_err(|e| (false, e))?;
                let message = p.input(&[0; 640]);
                write_active(
                    shared,
                    ws,
                    message,
                    end.min(authority)
                        .min(Instant::now() + Duration::from_millis(500)),
                    stop,
                    global,
                )
                .await
                .map_err(|e| (true, e))?;
                // Anchor to actual completion: a delayed task never catches up
                // by bursting multiple clock frames into the provider.
                next_clock = Some(Instant::now() + Duration::from_millis(20));
            }
            Wake::InputInterrupted => {
                return Err((
                    false,
                    if shared.input_idle_grace > Duration::from_secs(2) {
                        ProviderError::InputInterrupted
                    } else {
                        ProviderError::Deadline
                    },
                ));
            }
            Wake::Timeout => {
                if p.ready && Instant::now() >= authority {
                    continue; // top-of-loop records observation cutoff when draining
                }
                if p.route == Route::Transcription {
                    p.asr.check(p.frames).map_err(|e| (false, e))?;
                }
                return Err((
                    false,
                    if p.ready {
                        ProviderError::Deadline
                    } else {
                        ProviderError::ConnectionLost
                    },
                ));
            }
            Wake::Input(None) => return Ok(()),
            Wake::Input(Some(StreamInput::Pcm16(bytes))) => {
                // A bounded number of frames may already be queued when the
                // supervisor observes InputSealed. The handover notice declares
                // this coverage gap; these frames are never replayed or committed.
                if boundary_handover && finishing.is_some() && bytes.len() == 640 {
                    sealed_discarded += 1;
                    if sealed_discarded > 8 {
                        return Err((false, ProviderError::ProtocolCapacityLimit));
                    }
                    continue;
                }
                if finishing.is_some() || bytes.len() != 640 {
                    return Err((false, ProviderError::Protocol));
                }
                let origin = *input_origin.get_or_insert_with(Instant::now);
                if p.captured_frames >= origin.elapsed().as_millis() as u64 / 20 + 10 {
                    return Err((false, ProviderError::InputPacing));
                }
                p.captured_frames += 1;
                shared.input(320).map_err(|e| (false, e))?;
                if let Some(vad) = local_vad.as_mut() {
                    let segment = vad.push(&bytes).map_err(|e| (false, boundary_error(e)))?;
                    write_local_segment(
                        shared,
                        ws,
                        p,
                        &mut converter,
                        &mut selected_end,
                        segment,
                        authority,
                        stop,
                        global,
                    )
                    .await?;
                    last_input = Instant::now();
                    received_audio = true;
                    if boundary_handover && !vad.active() {
                        let now = Instant::now();
                        finishing = Some((now, (now + Duration::from_secs(45)).min(authority)));
                        events
                            .try_send(StreamEvent::InputSealed)
                            .map_err(|_| (false, ProviderError::ProtocolCapacityLimit))?;
                    }
                    continue;
                }
                let converted = if p.route == Route::Live {
                    bytes
                } else {
                    converter.frame(&bytes).map_err(|e| (false, e))?
                };
                let message = p.input(&converted);
                write_active(
                    shared,
                    ws,
                    message,
                    authority.min(Instant::now() + Duration::from_millis(500)),
                    stop,
                    global,
                )
                .await
                .map_err(|e| (true, e))?;
                last_input = Instant::now();
                received_audio = true;
            }
            Wake::Input(Some(StreamInput::Text(text))) => {
                if finishing.is_some() {
                    return Err((false, ProviderError::Protocol));
                }
                shared.text(&text).map_err(|e| (false, e))?;
                let value = p.live_text(&text).map_err(|e| (false, e))?;
                write_active(
                    shared,
                    ws,
                    value,
                    authority.min(Instant::now() + Duration::from_millis(500)),
                    stop,
                    global,
                )
                .await
                .map_err(|e| (true, e))?;
            }
            Wake::Input(Some(StreamInput::FinishAtBoundary)) => {
                if boundary_handover || finishing.is_some() || p.route != Route::Transcription {
                    return Err((false, ProviderError::Protocol));
                }
                boundary_handover = true;
                if local_vad.as_ref().is_none_or(|vad| !vad.active()) {
                    // Inactive may still contain an incomplete one-frame onset.
                    // Finish verifies it; this branch never forces active speech.
                    local_vad
                        .as_mut()
                        .ok_or((false, ProviderError::Protocol))?
                        .finish()
                        .map_err(|e| (false, boundary_error(e)))?;
                    let now = Instant::now();
                    finishing = Some((now, (now + Duration::from_secs(45)).min(authority)));
                    events
                        .try_send(StreamEvent::InputSealed)
                        .map_err(|_| (false, ProviderError::ProtocolCapacityLimit))?;
                }
            }
            Wake::Input(Some(StreamInput::FinishInput)) => {
                // The explicit user barrier may arrive after a planned natural
                // seal. Adopt that drain without a second commit or receipt.
                if boundary_handover && finishing.is_some() {
                    continue;
                }
                if finishing.is_some() {
                    return Err((false, ProviderError::Protocol));
                }
                if let Some(vad) = local_vad.as_mut() {
                    let segment = vad.finish().map_err(|e| (false, boundary_error(e)))?;
                    write_local_segment(
                        shared,
                        ws,
                        p,
                        &mut converter,
                        &mut selected_end,
                        segment,
                        authority,
                        stop,
                        global,
                    )
                    .await?;
                }
                let now = Instant::now();
                let end = (now + Duration::from_secs(20)).min(authority);
                finishing = Some((now, end));
                if p.route == Route::Translation {
                    // The documented translation terminal flush is a command,
                    // not a wall-clock pause. Record before a possible partial
                    // write so cleanup can never repeat it.
                    p.close_requested = true;
                    write_active(
                        shared,
                        ws,
                        json!({"type":"session.close"}),
                        end.min(Instant::now() + Duration::from_millis(500)),
                        stop,
                        global,
                    )
                    .await
                    .map_err(|e| (true, e))?;
                } else if p.route == Route::Live {
                    next_clock = Some(now + Duration::from_millis(20));
                }
            }
            Wake::Incoming(Some(Ok(Message::Text(text)))) => {
                let value: Value = serde_json::from_str(&text).map_err(|_| {
                    p.failure
                        .get_or_insert((Event::Other, Category::InvalidJson));
                    (false, ProviderError::Protocol)
                })?;
                let output = p.consume(value).map_err(|e| (false, e))?;
                if p.route == Route::Transcription {
                    shared
                        .text_count(std::mem::take(&mut p.asr.text_charge))
                        .map_err(|e| (false, e))?;
                }
                if *stop.borrow()
                    || *global.borrow()
                    || shared.revocation.revoked()
                    || (!admitted && shared.admission.revoked())
                {
                    return Ok(());
                }
                if Instant::now() >= authority
                    || finishing.is_some_and(|(_, end)| Instant::now() >= end)
                {
                    continue 'drive; // preserve terminal receipt, publish only cutoff
                }
                if p.closed {
                    if p.route == Route::Live && finishing.is_none() && !p.close_requested {
                        let error = ProviderError::SessionEnded(
                            p.live_end_reason.ok_or((false, ProviderError::Protocol))?,
                        );
                        p.failure
                            .get_or_insert((Event::SessionClosed, error_category(error)));
                        // A valid unsolicited terminal receipt confirms closure,
                        // not normal local completion or permission to retry.
                        return Err((false, error));
                    }
                    if p.route == Route::Translation
                        && p.close_requested
                        && finishing.is_some()
                        && !*stop.borrow()
                        && !*global.borrow()
                        && Instant::now() < authority
                    {
                        events
                            .try_send(StreamEvent::Drained {
                                completion: DrainCompletion::ProviderSessionClosed,
                            })
                            .map_err(|_| (false, ProviderError::ProtocolCapacityLimit))?;
                    }
                    return Ok(());
                }
                if p.created && !configured {
                    configured = true;
                    write_guarded(
                        shared,
                        ws,
                        p.startup(),
                        ready_end,
                        stop,
                        global,
                        (!admitted).then_some(&shared.admission),
                    )
                    .await
                    .map_err(|e| (true, e))?;
                }
                for mut event in output {
                    if matches!(
                        event,
                        StreamEvent::Audio { .. }
                            | StreamEvent::Transcript { .. }
                            | StreamEvent::LiveTranscript { .. }
                    ) {
                        last_output = Instant::now();
                    }
                    if *stop.borrow()
                        || *global.borrow()
                        || shared.revocation.revoked()
                        || (!admitted && shared.admission.revoked())
                    {
                        return Ok(());
                    }
                    if Instant::now() >= authority
                        || finishing.is_some_and(|(_, end)| Instant::now() >= end)
                    {
                        continue 'drive;
                    }
                    if let StreamEvent::Ready { expires_in_seconds } = &mut event {
                        *expires_in_seconds = p
                            .expiry
                            .map(|e| e - Duration::from_secs(10))
                            .unwrap_or(authority)
                            .min(authority)
                            .saturating_duration_since(Instant::now())
                            .as_secs();
                    }
                    match &event {
                        StreamEvent::Audio { pcm16, sample_rate } => shared
                            .output(pcm16.len(), *sample_rate)
                            .map_err(|e| (false, e))?,
                        StreamEvent::Transcript {
                            text, provenance, ..
                        } if *provenance != Provenance::SourceAsr => {
                            shared.text(text).map_err(|e| (false, e))?
                        }
                        StreamEvent::LiveTranscript { text, .. } => {
                            shared.text(text).map_err(|e| (false, e))?
                        }
                        _ => {}
                    }
                    let releases_ready = matches!(event, StreamEvent::Ready { .. });
                    events
                        .try_send(event)
                        .map_err(|_| (false, ProviderError::ProtocolCapacityLimit))?;
                    if releases_ready {
                        admitted = true;
                    }
                }
            }
            Wake::Incoming(Some(Ok(Message::Ping(data)))) => {
                guard::until_admitted(
                    authority.min(Instant::now() + Duration::from_millis(500)),
                    stop,
                    global,
                    &shared.revocation,
                    (!admitted).then_some(&shared.admission),
                    ws.send(Message::Pong(data)),
                )
                .await
                .map_err(|e| (true, e))?
                .map_err(|error| (true, transport_error(&error)))?;
            }
            Wake::Incoming(Some(Ok(Message::Pong(_)))) => {}
            Wake::Incoming(Some(Err(error))) => return Err((true, transport_error(&error))),
            Wake::Incoming(None | Some(Ok(Message::Close(_)))) => {
                return Err((true, ProviderError::ConnectionLost));
            }
            Wake::Incoming(_) => return Err((true, ProviderError::Protocol)),
        }
    }
}
#[allow(clippy::too_many_arguments)]
async fn write_local_segment<S: AsyncRead + AsyncWrite + Unpin>(
    shared: &Shared,
    ws: &mut WebSocketStream<S>,
    p: &mut Protocol,
    converter: &mut audio::Upsample,
    selected_end: &mut Option<u64>,
    segment: super::local_vad::Segment,
    authority: Instant,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
) -> Result<(), (bool, ProviderError)> {
    if !segment.append.is_empty() {
        if segment.append.len() > 9600
            || !segment.append.len().is_multiple_of(640)
            || segment.end_frame.checked_sub(segment.start_frame)
                != Some((segment.append.len() / 640) as u64)
        {
            return Err((false, ProviderError::Protocol));
        }
        if *selected_end != Some(segment.start_frame) {
            *converter = audio::Upsample::new();
        }
        let mut converted = Vec::with_capacity(segment.append.len() * 3 / 2);
        for frame in segment.append.chunks_exact(640) {
            converted.extend(converter.frame(frame).map_err(|e| (false, e))?);
        }
        p.asr
            .append_local(segment.start_frame, segment.end_frame)
            .map_err(|e| (false, e))?;
        let message = p.input(&converted);
        write_active(
            shared,
            ws,
            message,
            authority.min(Instant::now() + Duration::from_millis(500)),
            stop,
            global,
        )
        .await
        .map_err(|e| (true, e))?;
        *selected_end = Some(segment.end_frame);
    }
    if let Some(boundary) = segment.commit {
        let value = p.asr.commit_local(boundary).map_err(|e| (false, e))?;
        write_active(
            shared,
            ws,
            value,
            authority.min(Instant::now() + Duration::from_millis(500)),
            stop,
            global,
        )
        .await
        .map_err(|e| (true, e))?;
    }
    Ok(())
}
async fn write_active<S: AsyncRead + AsyncWrite + Unpin>(
    shared: &Shared,
    ws: &mut WebSocketStream<S>,
    value: Value,
    at: Instant,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
) -> Result<(), ProviderError> {
    write_guarded(shared, ws, value, at, stop, global, None).await
}
async fn write_guarded<S: AsyncRead + AsyncWrite + Unpin>(
    shared: &Shared,
    ws: &mut WebSocketStream<S>,
    value: Value,
    at: Instant,
    stop: &mut watch::Receiver<bool>,
    global: &mut watch::Receiver<bool>,
    admission: Option<&guard::ExternalRevocation>,
) -> Result<(), ProviderError> {
    guard::until_admitted(
        at,
        stop,
        global,
        &shared.revocation,
        admission,
        ws.send(Message::Text(value.to_string().into())),
    )
    .await
    .map_err(|error| {
        if error == ProviderError::Deadline
            && !*stop.borrow()
            && !*global.borrow()
            && !shared.revocation.revoked()
            && admission.is_none_or(|f| !f.revoked())
        {
            ProviderError::ConnectionLost
        } else {
            error
        }
    })?
    .map_err(|error| transport_error(&error))
}
pub(super) async fn close<S: AsyncRead + AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
    p: &mut Protocol,
    at: Instant,
) -> bool {
    if p.closed {
        return true;
    }
    let protocol_end = (Instant::now() + Duration::from_secs(7)).min(at);
    if p.route != Route::Transcription && !p.close_requested {
        p.close_requested = true;
        if !cleanup_write(ws, json!({"type":"session.close"}), protocol_end).await {
            return false;
        }
    }
    let manual = p.route == Route::Transcription && p.asr.is_local();
    while !p.closed
        && (p.route != Route::Transcription
            || if manual {
                p.asr.awaiting_local_finals()
            } else {
                !p.complete_input()
            })
    {
        match deadline(protocol_end, ws.next()).await {
            Ok(Some(Ok(Message::Text(text)))) => {
                let Ok(value) = serde_json::from_str(&text) else {
                    p.failure
                        .get_or_insert((Event::Other, Category::InvalidJson));
                    return false;
                };
                if p.consume(value).is_err() {
                    if manual {
                        break;
                    } else {
                        return false;
                    }
                }
            }
            Ok(Some(Ok(Message::Ping(bytes)))) => {
                if !matches!(
                    deadline(protocol_end, ws.send(Message::Pong(bytes))).await,
                    Ok(Ok(()))
                ) {
                    return false;
                }
            }
            Ok(Some(Ok(Message::Pong(_)))) => {}
            _ => {
                if manual {
                    break;
                } else {
                    return false;
                }
            }
        }
    }
    if p.route != Route::Transcription {
        return p.closed;
    }
    // ASR has no session.closed event. After rejection of a valid-created
    // zero-input session there is no audio to finalize. It still owns a socket
    // that must complete the normal close handshake; this never grants Ready or
    // clears the original error. An unrecognized session cannot establish
    // confirmed retirement. Incomplete source content remains separately
    // unverified and poisons further work in run(), even after a peer close.
    let known_session = p.ready || (p.created && p.frames == 0 && p.complete_input());
    if !matches!(deadline(at, ws.close(None)).await, Ok(Ok(()))) {
        return false;
    }
    loop {
        match deadline(at, ws.next()).await {
            Ok(Some(Ok(Message::Close(_)))) => return known_session,
            Ok(Some(Ok(Message::Pong(_)))) => {}
            _ => return false,
        }
    }
}
async fn cleanup_write<S: AsyncRead + AsyncWrite + Unpin>(
    ws: &mut WebSocketStream<S>,
    value: Value,
    at: Instant,
) -> bool {
    matches!(
        deadline(at, ws.send(Message::Text(value.to_string().into()))).await,
        Ok(Ok(()))
    )
}
async fn hangup(shared: &Shared, id: &str, at: Instant) -> Result<(), ProviderError> {
    if !protocol::safe_id(id) {
        return Err(ProviderError::Protocol);
    }
    let client = http()?;
    let request = client
        .post(format!(
            "https://api.openai.com/v1/live/sessions/{id}/hangup"
        ))
        .header("Authorization", auth(&shared.config.api_key)?);
    let response = deadline(at, request.send())
        .await?
        .map_err(|_| ProviderError::Unavailable)?;
    if response.status().is_success() {
        Ok(())
    } else {
        Err(ProviderError::Unavailable)
    }
}
pub(super) async fn synthesize(
    shared: Arc<Shared>,
    text: String,
    language: Language,
) -> Result<PreparedAudio, ProviderError> {
    // Defend direct backend callers too, before any reservation, text charge,
    // client construction or external request. TTS scope was not expanded.
    if !language.legacy() {
        return Err(ProviderError::Protocol);
    }
    let body = protocol::tts_request(&text)?;
    let mut reservation = shared.reserve(None, text.len())?;
    shared.text(&text)?;
    let mut global = shared.stop.subscribe();
    let (_local_tx, mut local) = watch::channel(false);
    let at =
        (Instant::now() + Duration::from_secs(65)).min(shared.deadline - Duration::from_secs(10));
    let client = http()?;
    let request = client
        .post("https://api.openai.com/v1/audio/speech")
        .header("Authorization", auth(&shared.config.api_key)?)
        .header("Content-Type", "application/json")
        .body(body.to_string());
    let mut response = guard::until_admitted(
        at,
        &mut local,
        &mut global,
        &shared.revocation,
        Some(&shared.admission),
        request.send(),
    )
    .await?
    .map_err(|_| ProviderError::Unavailable)?;
    if !response.status().is_success() {
        return Err(ProviderError::Unavailable);
    }
    let content_type = response
        .headers()
        .get("Content-Type")
        .and_then(|v| v.to_str().ok())
        .unwrap_or("");
    if !matches!(
        content_type.split(';').next().map(str::trim),
        Some("audio/pcm" | "application/octet-stream")
    ) {
        return Err(ProviderError::Protocol);
    }
    if response
        .content_length()
        .is_some_and(|n| n > audio::MAX_TTS_PCM as u64)
    {
        return Err(ProviderError::Protocol);
    }
    let mut bytes = Vec::new();
    loop {
        let chunk = guard::until_admitted(
            at,
            &mut local,
            &mut global,
            &shared.revocation,
            Some(&shared.admission),
            response.chunk(),
        )
        .await?
        .map_err(|_| ProviderError::Unavailable)?;
        let Some(chunk) = chunk else { break };
        if bytes.len() + chunk.len() > audio::MAX_TTS_PCM {
            return Err(ProviderError::OutputAudioLimit);
        }
        bytes.extend_from_slice(&chunk);
    }
    shared.output(bytes.len(), audio::TTS_RATE)?;
    let result = audio::wav(bytes)?;
    if !shared.available() || Instant::now() >= at {
        return Err(ProviderError::Deadline);
    }
    reservation.completed = true;
    Ok(result)
}
