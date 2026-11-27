//! Finite, source-launcher Gemini lane. Both routes use the same fixed Google
//! websocket destination and backend-specific key. No URL/key discovery,
//! redirects, reconnection, session resumption or tools.
use super::{
    DiagnosticCategory, DiagnosticEvent, DiagnosticStage, ProviderDiagnostic, Reservation, Shared,
    diagnostics::error_category, guard, transport,
};
use crate::engine::{
    DrainCompletion, Interpretation, Language, LiveMode, ProviderError, Route, SpeechHints,
    StreamEvent, StreamHandle, StreamInput,
};
use crate::interpretation_context::InterpretationContext;
use base64::{Engine, engine::general_purpose::STANDARD};
use futures_util::{SinkExt, StreamExt};
use serde_json::{Value, json};
use std::{
    future::Future,
    sync::Arc,
    time::{Duration, Instant},
};
use tokio::{
    io::{AsyncRead, AsyncWrite},
    sync::{mpsc, watch},
};
use tokio_tungstenite::{
    Connector, WebSocketStream, connect_async_tls_with_config,
    tungstenite::{
        Message, client::IntoClientRequest, handshake::client::Request, http::HeaderValue,
    },
};
mod protocol;
use protocol::Protocol;
#[cfg(test)]
mod tests;
#[cfg(test)]
pub(super) type FixturePeers = mpsc::Sender<(Route, WebSocketStream<tokio::io::DuplexStream>)>;

const ENDPOINT: &str = "wss://generativelanguage.googleapis.com/ws/google.ai.generativelanguage.v1beta.GenerativeService.BidiGenerateContent";
const START_TIMEOUT: Duration = Duration::from_secs(5);
const WRITE_TIMEOUT: Duration = Duration::from_secs(2);
const DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

fn handshake(key: &str) -> Result<Request, ProviderError> {
    // Reject an obviously misplaced OpenAI secret even if a caller mislabeled
    // it. Provider-specific activation supplies the key; no other key is read.
    if key.starts_with("sk-") {
        return Err(ProviderError::Unavailable);
    }
    let mut request = ENDPOINT
        .into_client_request()
        .map_err(|_| ProviderError::Unavailable)?;
    let mut key = HeaderValue::from_str(key).map_err(|_| ProviderError::Unavailable)?;
    key.set_sensitive(true);
    request.headers_mut().insert("x-goog-api-key", key);
    Ok(request)
}

pub(super) async fn open(
    shared: Arc<Shared>,
    route: Route,
    language: Language,
    mode: Option<LiveMode>,
    hints: Option<SpeechHints>,
    policy: Option<Interpretation>,
    context: Option<InterpretationContext>,
) -> Result<StreamHandle, ProviderError> {
    let protocol = Protocol::new(route, language, mode, hints, policy, context)?;
    if route == Route::Live
        && !shared
            .config
            .allowed_live_modes
            .contains(&LiveMode::Interpretation)
    {
        return Err(ProviderError::Unavailable);
    }
    let reservation = shared.reserve(Some(route), 0)?;
    let (input, rx) = mpsc::channel(8);
    let (tx, events) = mpsc::channel(32);
    let (stop, stopped) = watch::channel(false);
    let owned = shared.clone();
    shared.spawn_owned(async move {
        #[cfg(test)]
        if let Some(peers) = owned.gemini_peers.clone() {
            let connect = async move |_: Request| {
                let (socket, peer) = tests::peer_pair().await;
                peers
                    .send((route, peer))
                    .await
                    .map_err(|_| (ProviderError::Unavailable, None))?;
                Ok(socket)
            };
            run(owned, reservation, protocol, rx, tx, stopped, connect).await;
            return;
        }
        run(
            owned,
            reservation,
            protocol,
            rx,
            tx,
            stopped,
            connect_google,
        )
        .await;
    })?;
    Ok(StreamHandle {
        input,
        events,
        stop,
        renewal_fence: None,
    })
}

struct Scope<'a> {
    shared: &'a Shared,
    stop: watch::Receiver<bool>,
    global: watch::Receiver<bool>,
    hard: Instant,
}
impl Scope<'_> {
    async fn guarded<F: Future>(
        &mut self,
        end: Instant,
        startup: bool,
        task: F,
    ) -> Result<F::Output, ProviderError> {
        guard::until_admitted(
            end.min(self.hard),
            &mut self.stop,
            &mut self.global,
            &self.shared.revocation,
            startup.then_some(&self.shared.admission),
            task,
        )
        .await
    }
    fn cancelled(&self) -> bool {
        *self.stop.borrow() || *self.global.borrow() || self.shared.revocation.revoked()
    }
}

async fn connect_google(
    request: Request,
) -> Result<
    WebSocketStream<tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>>,
    (ProviderError, Option<u16>),
> {
    let tls = transport::tls().map_err(|error| (error, None))?;
    connect_async_tls_with_config(
        request,
        Some(transport::ws_config()),
        true,
        Some(Connector::Rustls(Arc::new(tls))),
    )
    .await
    .map(|(socket, _)| socket)
    .map_err(|error| {
        (
            transport::transport_error(&error),
            transport::handshake_status(&error),
        )
    })
}

async fn run<S: AsyncRead + AsyncWrite + Unpin>(
    shared: Arc<Shared>,
    mut reservation: Reservation,
    mut p: Protocol,
    input: mpsc::Receiver<StreamInput>,
    events: mpsc::Sender<StreamEvent>,
    stop: watch::Receiver<bool>,
    dial: impl AsyncFnOnce(Request) -> Result<WebSocketStream<S>, (ProviderError, Option<u16>)>,
) {
    let hard = (Instant::now() + Duration::from_secs(reservation.seconds)).min(shared.deadline);
    let mut scope = Scope {
        global: shared.stop.subscribe(),
        shared: &shared,
        stop,
        hard,
    };
    let route = p.route;
    let mut status = None;
    let connect = async {
        let request = handshake(&shared.config.api_key)?;
        dial(request).await.map_err(|(error, http_status)| {
            status = http_status;
            error
        })
    };
    let connected = scope
        .guarded(Instant::now() + START_TIMEOUT, true, connect)
        .await
        .and_then(|x| x);
    let (stage, category, confirmed) = match connected {
        Ok(mut socket) => {
            let (result, confirmed) = drive(&mut scope, &mut socket, &mut p, input, &events).await;
            if let Err(error) = result {
                let _ = events.try_send(StreamEvent::Error(error));
                (
                    if p.ready {
                        DiagnosticStage::Streaming
                    } else {
                        DiagnosticStage::Startup
                    },
                    error_category(error),
                    confirmed,
                )
            } else if confirmed {
                (
                    DiagnosticStage::Complete,
                    DiagnosticCategory::Confirmed,
                    true,
                )
            } else {
                (
                    DiagnosticStage::Closing,
                    DiagnosticCategory::CloseUnconfirmed,
                    false,
                )
            }
        }
        Err(error) => {
            let _ = events.try_send(StreamEvent::Error(error));
            (
                DiagnosticStage::Handshake,
                if status.is_some() {
                    DiagnosticCategory::HttpRefused
                } else {
                    error_category(error)
                },
                false,
            )
        }
    };
    // Losing a close receipt consumes the entire local reservation and fences
    // further opens. A clean peer close is not a claim about billed usage.
    reservation.completed = confirmed;
    if !confirmed {
        shared.poison();
    }
    shared.record_diagnostic(ProviderDiagnostic {
        route,
        stage,
        category,
        event_type: DiagnosticEvent::None,
        upstream_code: Default::default(),
        upstream_parameter: Default::default(),
        http_status: status,
        ready: p.ready,
        closed_confirmed: confirmed,
    });
    let _ = events.try_send(StreamEvent::Closed { confirmed });
}

struct Wire<'a, S> {
    socket: &'a mut WebSocketStream<S>,
    // Cancelling a partially polled application write may leave a queued audio
    // frame. Never flush or send Close after an uncertain write.
    dirty: bool,
}
impl<S: AsyncRead + AsyncWrite + Unpin> Wire<'_, S> {
    async fn send(
        &mut self,
        scope: &mut Scope<'_>,
        value: Value,
        startup: bool,
    ) -> Result<(), ProviderError> {
        self.dirty = true;
        scope
            .guarded(
                Instant::now() + WRITE_TIMEOUT,
                startup,
                self.socket.send(Message::Text(value.to_string().into())),
            )
            .await?
            .map_err(|_| ProviderError::ConnectionLost)?;
        self.dirty = false;
        Ok(())
    }
    async fn send_audio(
        &mut self,
        scope: &mut Scope<'_>,
        audio: &mut Vec<u8>,
        protocol: &mut Protocol,
    ) -> Result<(), ProviderError> {
        if audio.is_empty() {
            return Ok(());
        }
        let frames = (audio.len() / 640) as u32;
        self.send(
            scope,
            json!({"realtimeInput":{"audio":{
                "mimeType":"audio/pcm;rate=16000", "data":STANDARD.encode(&*audio)
            }}}),
            false,
        )
        .await?;
        protocol.frames += frames;
        audio.clear();
        Ok(())
    }
    async fn close(&mut self) -> bool {
        if self.dirty {
            return false;
        }
        let result = transport::deadline(Instant::now() + WRITE_TIMEOUT, async {
            if self.socket.send(Message::Close(None)).await.is_err() {
                return false;
            }
            // Ignore already-in-flight output. Only an actual peer close frame
            // is evidence; transport EOF, reset and silence are uncertain.
            for _ in 0..32 {
                match self.socket.next().await {
                    Some(Ok(Message::Close(_))) => return true,
                    Some(Ok(_)) => {}
                    _ => return false,
                }
            }
            false
        })
        .await;
        result.unwrap_or(false)
    }
}

async fn drive<S: AsyncRead + AsyncWrite + Unpin>(
    scope: &mut Scope<'_>,
    socket: &mut WebSocketStream<S>,
    p: &mut Protocol,
    input: mpsc::Receiver<StreamInput>,
    events: &mpsc::Sender<StreamEvent>,
) -> (Result<(), ProviderError>, bool) {
    let mut wire = Wire {
        socket,
        dirty: false,
    };
    let result = stream(scope, &mut wire, p, input, events).await;
    let requested = scope.cancelled();
    let confirmed = match result {
        Ok(Exit::PeerClosed) => true,
        _ => wire.close().await,
    };
    let result = match result {
        Ok(Exit::PeerClosed) if !requested => Err(ProviderError::ConnectionLost),
        Ok(_) => Ok(()),
        Err(ProviderError::Deadline) if requested => Ok(()),
        Err(error) => Err(error),
    };
    (result, confirmed)
}
enum Exit {
    Local,
    PeerClosed,
}

async fn stream<S: AsyncRead + AsyncWrite + Unpin>(
    scope: &mut Scope<'_>,
    wire: &mut Wire<'_, S>,
    p: &mut Protocol,
    mut input: mpsc::Receiver<StreamInput>,
    events: &mpsc::Sender<StreamEvent>,
) -> Result<Exit, ProviderError> {
    let start_limit = Instant::now() + START_TIMEOUT;
    wire.send(scope, p.setup.clone(), true).await?;
    let mut first_audio = None;
    let mut last_audio = Instant::now();
    let mut drain = None;
    // The local capture contract is 20 ms; Google recommends 100 ms packets.
    // Only explicit Finish seals a short tail. Stop drops buffered audio.
    const PACKET_BYTES: usize = 5 * 640;
    let mut packet = Vec::with_capacity(PACKET_BYTES);
    let mut captured = 0u32;
    enum Wake {
        Peer(Option<tokio_tungstenite::tungstenite::Result<Message>>),
        Input(Option<StreamInput>),
        Drain,
        Idle,
    }
    loop {
        let hard = scope.hard;
        let idle = last_audio + scope.shared.input_idle_grace;
        let wake = scope.guarded(if p.ready { hard } else { start_limit }, !p.ready, async {
            tokio::select! {
                biased;
                _ = tokio::time::sleep_until(drain.unwrap_or(hard).into()), if drain.is_some() => Wake::Drain,
                _ = tokio::time::sleep_until(idle.into()), if captured > 0 && drain.is_none() => Wake::Idle,
                message = wire.socket.next() => Wake::Peer(message),
                frame = input.recv(), if p.ready && drain.is_none() => Wake::Input(frame),
            }
        }).await?;
        match wake {
            Wake::Input(Some(StreamInput::Pcm16(bytes))) => {
                if bytes.len() != 640 {
                    return Err(ProviderError::Protocol);
                }
                let origin = first_audio.get_or_insert_with(Instant::now);
                if u128::from(captured) >= origin.elapsed().as_millis() / 20 + 10 {
                    return Err(ProviderError::InputPacing);
                }
                scope.shared.input(320)?;
                captured += 1;
                packet.extend_from_slice(&bytes);
                if packet.len() == PACKET_BYTES {
                    wire.send_audio(scope, &mut packet, p).await?;
                }
                last_audio = Instant::now();
            }
            Wake::Input(Some(StreamInput::FinishInput)) => {
                wire.send_audio(scope, &mut packet, p).await?;
                wire.send(
                    scope,
                    json!({"realtimeInput":{"audioStreamEnd":true}}),
                    false,
                )
                .await?;
                drain = Some(Instant::now() + DRAIN_TIMEOUT);
            }
            Wake::Input(Some(StreamInput::Text(_) | StreamInput::FinishAtBoundary)) => {
                return Err(ProviderError::Unavailable);
            }
            Wake::Input(None) => return Ok(Exit::Local),
            Wake::Drain => {
                // Neither turnComplete nor an ASR final is a session-wide flush
                // receipt for every submitted frame of the entire audio stream.
                events
                    .try_send(StreamEvent::Drained {
                        completion: DrainCompletion::ObservationWindowEnded,
                    })
                    .map_err(|_| ProviderError::ProtocolCapacityLimit)?;
                return Ok(Exit::Local);
            }
            Wake::Idle => return Err(ProviderError::InputInterrupted),
            Wake::Peer(Some(Ok(message @ (Message::Text(_) | Message::Binary(_))))) => {
                // The API can send the same UTF-8 JSON envelope with either
                // WebSocket data opcode. Audio is base64 inside that envelope.
                let bytes = message.into_data();
                if bytes.len() > 65_536 {
                    return Err(ProviderError::Protocol);
                }
                let value = serde_json::from_slice(&bytes).map_err(|_| ProviderError::Protocol)?;
                let starting = !p.ready;
                let output = p.consume(scope.shared, value)?;
                if starting {
                    let seconds = scope
                        .hard
                        .saturating_duration_since(Instant::now())
                        .as_secs();
                    scope
                        .guarded(start_limit, true, async {
                            events
                                .try_send(StreamEvent::Ready {
                                    expires_in_seconds: seconds,
                                })
                                .map_err(|_| ProviderError::ProtocolCapacityLimit)
                        })
                        .await??;
                }
                for event in output {
                    scope
                        .guarded(hard, false, async {
                            events
                                .try_send(event)
                                .map_err(|_| ProviderError::ProtocolCapacityLimit)
                        })
                        .await??;
                }
            }
            Wake::Peer(Some(Ok(Message::Ping(_)))) => {
                wire.dirty = true;
                scope
                    .guarded(
                        Instant::now() + WRITE_TIMEOUT,
                        !p.ready,
                        wire.socket.flush(),
                    )
                    .await?
                    .map_err(|_| ProviderError::ConnectionLost)?;
                wire.dirty = false;
            }
            Wake::Peer(Some(Ok(Message::Pong(_)))) => {}
            Wake::Peer(Some(Ok(Message::Close(_)))) => return Ok(Exit::PeerClosed),
            Wake::Peer(Some(Ok(_))) => return Err(ProviderError::Protocol),
            Wake::Peer(_) => return Err(ProviderError::ConnectionLost),
        }
    }
}
