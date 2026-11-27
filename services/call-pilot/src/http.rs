use crate::{
    auth::{self, ApiError, Role},
    engine::{Language, LiveMode, Route},
    runtime::{Pilot, Source},
};
use axum::{
    Json, Router,
    body::{Body, Bytes},
    extract::{
        DefaultBodyLimit, Extension, Path, State, WebSocketUpgrade,
        ws::{Message, WebSocket},
    },
    http::{HeaderMap, HeaderValue, Method, Request, StatusCode, header},
    middleware::{self, Next},
    response::{IntoResponse, Response},
    routing::{get, post},
};
use futures_util::SinkExt;
use serde::Deserialize;
use serde_json::{Value, json};
use std::{
    future::Future,
    path::{Path as FilePath, PathBuf},
    sync::Arc,
    task::Poll,
    time::{Duration, Instant},
};
use tower::ServiceExt;

#[derive(Clone)]
struct HttpState {
    pilot: Arc<Pilot>,
    assets: PathBuf,
    ordinary: Arc<tokio::sync::Semaphore>,
    control: Arc<tokio::sync::Semaphore>,
    surface: Role,
}

#[derive(Clone)]
pub(crate) struct RequestLimits {
    ordinary: Arc<tokio::sync::Semaphore>,
    control: Arc<tokio::sync::Semaphore>,
}
impl Default for RequestLimits {
    fn default() -> Self {
        Self {
            ordinary: Arc::new(tokio::sync::Semaphore::new(16)),
            control: Arc::new(tokio::sync::Semaphore::new(4)),
        }
    }
}

pub fn router(pilot: Arc<Pilot>, assets: PathBuf) -> Router {
    surface_router(pilot, assets, Role::Operator)
}
pub fn public_router(pilot: Arc<Pilot>, assets: PathBuf) -> Router {
    surface_router(pilot, assets, Role::Public)
}
fn surface_router(pilot: Arc<Pilot>, assets: PathBuf, surface: Role) -> Router {
    router_with_limits(pilot, assets, surface, RequestLimits::default())
}
pub(crate) fn router_with_limits(
    pilot: Arc<Pilot>,
    assets: PathBuf,
    surface: Role,
    limits: RequestLimits,
) -> Router {
    let state = HttpState {
        pilot,
        assets,
        ordinary: limits.ordinary,
        control: limits.control,
        surface,
    };
    Router::new()
        .route("/api/health", get(health))
        .route("/api/bootstrap", post(bootstrap))
        .route("/api/public/bootstrap", post(public_bootstrap))
        .route("/api/session", get(session))
        .route("/api/session/new", post(new_session))
        .route("/api/interpretation/state", get(interpretation_state))
        .route("/api/interpretation/pause", post(pause_interpretation))
        .route("/api/interpretation/resume", post(resume_interpretation))
        .route("/api/budget", get(budget))
        .route("/api/diagnostics", get(diagnostics))
        .route("/api/public/session", get(public_session))
        .route("/api/logout", post(logout))
        .route("/api/public/logout", post(public_logout))
        .route("/api/stop", post(stop))
        .route("/api/stop-receipt", get(stop_receipt))
        .route("/api/finish", post(finish))
        .route("/api/public/tickets", post(viewer_ticket))
        .route("/api/routes", post(start_route))
        .route("/api/routes/close", post(close_route))
        .route("/api/source-gap", post(source_gap))
        .route("/api/live/text", post(live_text))
        .route("/api/previews", post(preview))
        .route("/api/previews/{id}/audio", get(preview_audio))
        .route("/api/previews/{id}/audition", post(audition))
        .route("/api/previews/{id}/approve", post(approve))
        .route("/api/public/audio/{id}", get(public_audio))
        .route("/api/public/receipt", post(receipt))
        .route("/api/operator/events", get(operator_ws))
        .route("/api/public/events", get(public_ws))
        .route("/", get(index))
        .route("/public", get(index))
        .route("/assets/{*path}", get(asset))
        .fallback(|| async {
            (
                StatusCode::NOT_FOUND,
                Json(json!({"error":"invalid_request","generation":0})),
            )
        })
        .layer(DefaultBodyLimit::max(16 * 1024))
        .layer(middleware::from_fn_with_state(state.clone(), boundary))
        .with_state(state)
}

#[derive(Clone)]
struct HostSurface {
    host: Arc<crate::session_host::SessionHost>,
    role: Role,
}
/// Pin one complete router/Pilot per request. An old in-flight Stop or websocket
/// can never be retargeted to a newly created session.
pub fn hosted_router(host: Arc<crate::session_host::SessionHost>, role: Role) -> Router {
    Router::new()
        .fallback(host_dispatch)
        .with_state(HostSurface { host, role })
}
async fn host_dispatch(State(surface): State<HostSurface>, mut request: Request<Body>) -> Response {
    if request.uri().path().starts_with("/api/webrtc/") {
        return crate::webrtc::http::dispatch(surface.host, surface.role, request).await;
    }
    let current = surface.host.snapshot();
    request.extensions_mut().insert(surface.host);
    current
        .router(surface.role)
        .oneshot(request)
        .await
        .unwrap_or_else(|never| match never {})
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct NewSession {
    previous_generation: u64,
    request_id: String,
}
async fn new_session(
    State(state): State<HttpState>,
    host: Option<Extension<Arc<crate::session_host::SessionHost>>>,
    headers: HeaderMap,
    Json(body): Json<NewSession>,
) -> Response {
    let Some(Extension(host)) = host else {
        return fail(&state.pilot, ApiError::Forbidden);
    };
    match host
        .new_session(
            &state.pilot,
            headers,
            body.previous_generation,
            body.request_id,
        )
        .await
    {
        Ok(receipt) => session_response(&state.pilot, receipt.value(), &receipt.token),
        Err(error) => fail(&state.pilot, error),
    }
}

async fn resume_interpretation(
    State(state): State<HttpState>,
    host: Option<Extension<Arc<crate::session_host::SessionHost>>>,
    headers: HeaderMap,
    Json(body): Json<NewSession>,
) -> Response {
    let Some(Extension(host)) = host else {
        return fail(&state.pilot, ApiError::Forbidden);
    };
    match host
        .resume_interpretation(
            &state.pilot,
            headers,
            body.previous_generation,
            body.request_id,
        )
        .await
    {
        Ok(receipt) => session_response(&state.pilot, receipt.value(), &receipt.token),
        Err(error) => fail(&state.pilot, error),
    }
}

async fn interpretation_state(
    State(state): State<HttpState>,
    host: Option<Extension<Arc<crate::session_host::SessionHost>>>,
    headers: HeaderMap,
) -> Response {
    let Some(Extension(host)) = host else {
        return fail(&state.pilot, ApiError::Forbidden);
    };
    match host.interpretation_state(&state.pilot, &headers) {
        Ok(status) => Json(status).into_response(),
        Err(error) => fail(&state.pilot, error),
    }
}
async fn pause_interpretation(
    State(state): State<HttpState>,
    host: Option<Extension<Arc<crate::session_host::SessionHost>>>,
    headers: HeaderMap,
    Json(body): Json<Generation>,
) -> Response {
    let Some(Extension(host)) = host else {
        return fail(&state.pilot, ApiError::Forbidden);
    };
    match host.pause_interpretation(&state.pilot, &headers, body.generation) {
        Ok(status) => (StatusCode::ACCEPTED, Json(status)).into_response(),
        Err(error) => fail(&state.pilot, error),
    }
}

fn fail(pilot: &Pilot, error: ApiError) -> Response {
    (
        error.status(),
        Json(json!({"error":error.code(),"generation":pilot.generation()})),
    )
        .into_response()
}
fn result(pilot: &Pilot, result: Result<Value, ApiError>) -> Response {
    match result {
        Ok(value) => Json(value).into_response(),
        Err(error) => fail(pilot, error),
    }
}

async fn boundary(State(state): State<HttpState>, request: Request<Body>, next: Next) -> Response {
    let headers = request.headers();
    let post = request.method() == Method::POST;
    let websocket = headers.contains_key("upgrade") || request.uri().path().ends_with("/events");
    let origins = state.pilot.origins();
    let host = origins.host(state.surface);
    let origin = origins.origin(state.surface);
    let path = request.uri().path();
    let public_api = path.starts_with("/api/public/") && path != "/api/public/tickets";
    let allowed_surface = match state.surface {
        Role::Operator => !public_api && path != "/public",
        Role::Public => {
            public_api || path == "/public" || path == "/api/health" || path.starts_with("/assets/")
        }
    };
    let valid = allowed_surface
        && auth::single(headers, "host") == Some(host)
        && request.uri().query().is_none()
        && (!post && !websocket || auth::single(headers, "origin") == Some(origin))
        && (!headers.contains_key("origin") || auth::single(headers, "origin") == Some(origin))
        && (!post || auth::single(headers, "content-type") == Some("application/json"))
        && !headers.contains_key("forwarded")
        && !headers.contains_key("x-forwarded-host");
    let is_control = matches!(
        request.uri().path(),
        "/api/stop"
            | "/api/finish"
            | "/api/logout"
            | "/api/public/logout"
            | "/api/source-gap"
            | "/api/routes/close"
            | "/api/interpretation/pause"
    );
    let permits = if is_control {
        &state.control
    } else {
        &state.ordinary
    };
    let permit = permits.clone().try_acquire_owned();
    let long = request.uri().path() == "/api/previews";
    let rollover = matches!(
        request.uri().path(),
        "/api/session/new" | "/api/interpretation/resume"
    );
    let mut response = if !valid {
        fail(&state.pilot, ApiError::Forbidden)
    } else if let Ok(_permit) = permit {
        match tokio::time::timeout(
            Duration::from_secs(if long {
                32
            } else if rollover {
                18
            } else {
                2
            }),
            next.run(request),
        )
        .await
        {
            Ok(response) => response,
            Err(_) => fail(&state.pilot, ApiError::Unavailable),
        }
    } else {
        fail(&state.pilot, ApiError::Capacity)
    };
    // Framework JSON/path rejections may include attacker-controlled values.
    // Preserve rejection status while exposing only the fixed protocol error.
    if response.status().is_client_error()
        && response
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            != Some("application/json")
    {
        response = (
            response.status(),
            Json(json!({"error":"invalid_request","generation":state.pilot.generation()})),
        )
            .into_response();
    }
    let headers = response.headers_mut();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    headers.insert(
        "x-content-type-options",
        HeaderValue::from_static("nosniff"),
    );
    headers.insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    headers.insert(
        "content-security-policy",
        HeaderValue::from_static(origins.content_security_policy(state.surface)),
    );
    headers.insert(
        "cross-origin-opener-policy",
        HeaderValue::from_static(if state.surface == Role::Operator {
            "same-origin-allow-popups"
        } else {
            "unsafe-none"
        }),
    );
    headers.insert(
        "permissions-policy",
        HeaderValue::from_static(if state.surface == Role::Operator {
            "camera=(), microphone=(self), display-capture=(self)"
        } else {
            "camera=(), microphone=(), display-capture=()"
        }),
    );
    response
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TicketBody {
    ticket: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Generation {
    generation: u64,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRoute {
    generation: u64,
    source: Source,
    route: Route,
    language: Language,
    #[serde(default)]
    live_mode: Option<LiveMode>,
    #[serde(default)]
    transcription: Option<crate::engine::SpeechHints>,
    #[serde(default)]
    interpretation: Option<crate::engine::Interpretation>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseRoute {
    generation: u64,
    route_id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Gap {
    source: Source,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TextBody {
    generation: u64,
    route_id: String,
    text: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PreviewBody {
    generation: u64,
    text: String,
    language: Language,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct DigestBody {
    generation: u64,
    digest: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ReceiptBody {
    generation: u64,
    delivery_id: String,
    state: String,
}
#[derive(Deserialize)]
#[serde(tag = "type", rename_all = "snake_case", deny_unknown_fields)]
enum SocketControl {
    Heartbeat { generation: u64 },
    Finish { generation: u64 },
}

async fn health(State(s): State<HttpState>) -> Json<Value> {
    Json(
        json!({"mode":"supervised_local_pilot","providers_enabled":s.pilot.enabled(),"ready_for_real_call":false}),
    )
}
async fn bootstrap(State(s): State<HttpState>, Json(body): Json<TicketBody>) -> Response {
    redeem(&s.pilot, &body.ticket, Role::Operator)
}
async fn public_bootstrap(State(s): State<HttpState>, Json(body): Json<TicketBody>) -> Response {
    redeem(&s.pilot, &body.ticket, Role::Public)
}
fn redeem(pilot: &Pilot, ticket: &str, role: Role) -> Response {
    match pilot.redeem(ticket, role) {
        Ok((view, token)) => session_response(pilot, view, &token),
        Err(error) => fail(pilot, error),
    }
}
fn session_response(pilot: &Pilot, view: Value, token: &str) -> Response {
    let Ok(mut credential) = HeaderValue::from_str(token) else {
        return fail(pilot, ApiError::Unavailable);
    };
    credential.set_sensitive(true);
    let mut response = Json(view).into_response();
    response.headers_mut().insert("x-pilot-session", credential);
    response
}
async fn session(State(s): State<HttpState>, h: HeaderMap) -> Response {
    result(&s.pilot, s.pilot.session(&h, Role::Operator))
}
async fn public_session(State(s): State<HttpState>, h: HeaderMap) -> Response {
    result(&s.pilot, s.pilot.session(&h, Role::Public))
}
async fn stop(State(s): State<HttpState>, h: HeaderMap, Json(_): Json<Empty>) -> Response {
    if let Err(e) = s.pilot.authorize(&h, Role::Operator, true) {
        return fail(&s.pilot, e);
    }
    match s.pilot.operator_stop().await {
        Ok(generation) => Json(json!({"state":"stopped","generation":generation})).into_response(),
        Err(error) => fail(&s.pilot, error),
    }
}
async fn stop_receipt(State(s): State<HttpState>, h: HeaderMap, body: Bytes) -> Response {
    if !body.is_empty() {
        return fail(&s.pilot, ApiError::Invalid);
    }
    result(&s.pilot, s.pilot.stop_receipt(&h))
}
async fn finish(State(s): State<HttpState>, h: HeaderMap, Json(b): Json<Generation>) -> Response {
    match s
        .pilot
        .authorize(&h, Role::Operator, true)
        .and_then(|_| s.pilot.finish(b.generation))
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({"state":"finishing","generation":b.generation})),
        )
            .into_response(),
        Err(e) => fail(&s.pilot, e),
    }
}

async fn budget(State(s): State<HttpState>, h: HeaderMap, body: Bytes) -> Response {
    if !body.is_empty() {
        return fail(&s.pilot, ApiError::Invalid);
    }
    match s.pilot.budget(&h) {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => fail(&s.pilot, error),
    }
}
async fn diagnostics(State(s): State<HttpState>, h: HeaderMap, body: Bytes) -> Response {
    if !body.is_empty() {
        return fail(&s.pilot, ApiError::Invalid);
    }
    match s.pilot.diagnostics(&h) {
        Ok(snapshot) => Json(snapshot).into_response(),
        Err(error) => fail(&s.pilot, error),
    }
}
async fn logout(
    State(s): State<HttpState>,
    host: Option<Extension<Arc<crate::session_host::SessionHost>>>,
    h: HeaderMap,
    Json(_): Json<Empty>,
) -> Response {
    if let Some(Extension(host)) = host {
        if let Err(error) = host.logout(&s.pilot, &h) {
            return fail(&s.pilot, error);
        }
        return logged_out(&s.pilot, Role::Operator);
    }
    end_session(&s.pilot, &h, Role::Operator)
}
async fn public_logout(State(s): State<HttpState>, h: HeaderMap, Json(_): Json<Empty>) -> Response {
    end_session(&s.pilot, &h, Role::Public)
}
fn end_session(pilot: &Pilot, h: &HeaderMap, role: Role) -> Response {
    if let Err(e) = pilot.authorize(h, role, true) {
        return fail(pilot, e);
    }
    pilot.logout(role);
    logged_out(pilot, role)
}
fn logged_out(pilot: &Pilot, role: Role) -> Response {
    Json(json!({"state":if role==Role::Operator{"stopped"}else{"closed"},"generation":pilot.generation()})).into_response()
}
async fn viewer_ticket(
    State(s): State<HttpState>,
    h: HeaderMap,
    Json(b): Json<Generation>,
) -> Response {
    result(
        &s.pilot,
        s.pilot
            .authorize(&h, Role::Operator, true)
            .and_then(|_| s.pilot.public_ticket(b.generation)),
    )
}
async fn start_route(
    State(s): State<HttpState>,
    h: HeaderMap,
    Json(b): Json<StartRoute>,
) -> Response {
    match s.pilot.authorize(&h, Role::Operator, true).and_then(|_| {
        s.pilot.start_route_with_options(
            b.generation,
            b.source,
            b.route,
            b.language,
            crate::engine::RouteOptions {
                live_mode: b.live_mode,
                transcription: b.transcription,
                interpretation: b.interpretation,
            },
        )
    }) {
        Ok(id) => (
            StatusCode::ACCEPTED,
            Json(json!({"state":"starting","generation":s.pilot.generation(),"route_id":id})),
        )
            .into_response(),
        Err(e) => fail(&s.pilot, e),
    }
}
async fn close_route(
    State(s): State<HttpState>,
    h: HeaderMap,
    Json(b): Json<CloseRoute>,
) -> Response {
    let _stale_generation_does_not_block_close = b.generation;
    result(
        &s.pilot,
        s.pilot
            .authorize(&h, Role::Operator, true)
            .and_then(|_| s.pilot.close_route(&b.route_id))
            .map(|_| json!({"state":"closed","generation":s.pilot.generation()})),
    )
}
async fn source_gap(State(s): State<HttpState>, h: HeaderMap, Json(b): Json<Gap>) -> Response {
    if let Err(e) = s.pilot.authorize(&h, Role::Operator, true) {
        return fail(&s.pilot, e);
    }
    s.pilot.source_gap(b.source, "source_gap");
    Json(json!({"state":"closed","generation":s.pilot.generation()})).into_response()
}
async fn live_text(State(s): State<HttpState>, h: HeaderMap, Json(b): Json<TextBody>) -> Response {
    match s
        .pilot
        .authorize(&h, Role::Operator, true)
        .and_then(|_| s.pilot.live_text(b.generation, &b.route_id, b.text))
    {
        Ok(()) => (
            StatusCode::ACCEPTED,
            Json(json!({"state":"accepted","generation":s.pilot.generation()})),
        )
            .into_response(),
        Err(e) => fail(&s.pilot, e),
    }
}
async fn preview(State(s): State<HttpState>, h: HeaderMap, Json(b): Json<PreviewBody>) -> Response {
    if let Err(e) = s.pilot.authorize(&h, Role::Operator, true) {
        return fail(&s.pilot, e);
    }
    match s.pilot.prepare(b.generation, b.text, b.language).await {
        Ok(view) => (StatusCode::CREATED, Json(view)).into_response(),
        Err(e) => fail(&s.pilot, e),
    }
}
async fn audition(
    State(s): State<HttpState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<DigestBody>,
) -> Response {
    match s
        .pilot
        .authorize(&h, Role::Operator, true)
        .and_then(|_| s.pilot.audition(b.generation, &id, &b.digest))
    {
        Ok(view) => Json(view).into_response(),
        Err(e) => fail(&s.pilot, e),
    }
}
async fn approve(
    State(s): State<HttpState>,
    h: HeaderMap,
    Path(id): Path<String>,
    Json(b): Json<DigestBody>,
) -> Response {
    match s
        .pilot
        .authorize(&h, Role::Operator, true)
        .and_then(|_| s.pilot.approve(b.generation, &id, &b.digest))
    {
        Ok(id) => (
            StatusCode::ACCEPTED,
            Json(json!({"state":"offered","generation":s.pilot.generation(),"delivery_id":id})),
        )
            .into_response(),
        Err(e) => fail(&s.pilot, e),
    }
}
async fn preview_audio(
    State(s): State<HttpState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match s.pilot.authorize(&h, Role::Operator, false) {
        Ok(s) => s,
        Err(e) => return fail(&s.pilot, e),
    };
    let audio = s.pilot.preview_audio(&id);
    audio_response(s.pilot, session, Role::Operator, id, audio)
}
async fn public_audio(
    State(s): State<HttpState>,
    h: HeaderMap,
    Path(id): Path<String>,
) -> Response {
    let session = match s.pilot.authorize(&h, Role::Public, false) {
        Ok(s) => s,
        Err(e) => return fail(&s.pilot, e),
    };
    let audio = s.pilot.public_audio(&session, &id);
    audio_response(s.pilot, session, Role::Public, id, audio)
}
fn audio_response(
    pilot: Arc<Pilot>,
    session: String,
    role: Role,
    id: String,
    audio: Result<Arc<Vec<u8>>, ApiError>,
) -> Response {
    match audio {
        Ok(bytes) => {
            let chunks = futures_util::stream::unfold(
                (0usize, bytes, pilot, session, id),
                move |(offset, bytes, pilot, session, id)| async move {
                    if offset >= bytes.len() {
                        return None;
                    }
                    if !pilot.audio_current(&session, role, &id) {
                        let end = bytes.len();
                        return Some((
                            Err(std::io::Error::new(
                                std::io::ErrorKind::PermissionDenied,
                                "audio_revoked",
                            )),
                            (end, bytes, pilot, session, id),
                        ));
                    }
                    let end = (offset + 16 * 1024).min(bytes.len());
                    let chunk = bytes[offset..end].to_vec();
                    Some((Ok(chunk), (end, bytes, pilot, session, id)))
                },
            );
            (
                [(header::CONTENT_TYPE, "audio/wav")],
                Body::from_stream(chunks),
            )
                .into_response()
        }
        Err(e) => fail(&pilot, e),
    }
}
async fn receipt(State(s): State<HttpState>, h: HeaderMap, Json(b): Json<ReceiptBody>) -> Response {
    result(
        &s.pilot,
        s.pilot
            .authorize(&h, Role::Public, true)
            .and_then(|session| {
                s.pilot
                    .receipt(&session, b.generation, &b.delivery_id, &b.state)
            })
            .map(|_| json!({"state":"recorded","generation":s.pilot.generation()})),
    )
}
async fn operator_ws(State(s): State<HttpState>, h: HeaderMap, ws: WebSocketUpgrade) -> Response {
    upgrade(s, h, ws, Role::Operator)
}
async fn public_ws(State(s): State<HttpState>, h: HeaderMap, ws: WebSocketUpgrade) -> Response {
    upgrade(s, h, ws, Role::Public)
}
fn upgrade(s: HttpState, h: HeaderMap, ws: WebSocketUpgrade, role: Role) -> Response {
    let subscription = match s
        .pilot
        .authorize_events(&h, role)
        .and_then(|session| s.pilot.subscribe(session, role))
    {
        Ok(value) => value,
        Err(e) => return fail(&s.pilot, e),
    };
    let failed_pilot = s.pilot.clone();
    let failed_id = subscription.id.clone();
    ws.protocols([auth::EVENTS_PROTOCOL])
        .max_message_size(16 * 1024)
        .max_frame_size(16 * 1024)
        .on_failed_upgrade(move |_| failed_pilot.disconnected(&failed_id, role))
        .on_upgrade(move |socket| socket_loop(s.pilot, subscription, role, socket))
        .into_response()
}
async fn socket_loop(
    pilot: Arc<Pilot>,
    mut subscription: crate::runtime::Subscription,
    role: Role,
    mut socket: WebSocket,
) {
    let mut stopping = pilot.stop_receiver();
    let mut public_changed = pilot.public_receiver();
    let mut tick = tokio::time::interval(Duration::from_secs(1));
    let hello = json!({"type":"hello","generation":pilot.generation(),"role":role,"stopped":pilot.is_stopped()});
    if !send(&pilot, &subscription, role, &mut socket, hello).await {
        pilot.disconnected(&subscription.id, role);
        return;
    }
    loop {
        if !pilot.connection_current(&subscription.id, &subscription.session, role) {
            break;
        }
        tokio::select! { biased;
            _=stopping.changed()=>break,
            _=public_changed.changed(),if role==Role::Public=>break,
            _=tick.tick()=>{if !send(&pilot,&subscription,role,&mut socket,json!({"type":"heartbeat","generation":pilot.generation()})).await{break}},
            message=socket.recv()=>{match message {
                Some(Ok(Message::Text(text)))=>{
                    let Ok(control)=serde_json::from_str::<SocketControl>(&text)else{break};
                    let result=match control {
                        SocketControl::Heartbeat{generation}=>pilot.heartbeat(&subscription.id,role,generation),
                        SocketControl::Finish{generation} if role==Role::Operator=>pilot.finish_from_socket(generation,&subscription.id,&subscription.session),
                        _=>break,
                    };
                    if result.is_err(){break}
                },
                Some(Ok(Message::Binary(data))) if role==Role::Operator=>{
                    match pilot.frame(&data) {
                        Ok(()) if pilot.is_continuous() && pilot.heartbeat(&subscription.id,role,pilot.generation()).is_err()=>break,
                        Err(_) if pilot.is_stopped()||pilot.is_finishing()=>break,
                        _=>{},
                    }
                },
                Some(Ok(Message::Ping(_) | Message::Pong(_)))=>{},
                _=>break,
            }},
            event=subscription.events.recv()=>{let Some(event)=event else{break};if !send(&pilot,&subscription,role,&mut socket,event).await{break}},
        }
    }
    pilot.disconnected(&subscription.id, role);
    let _ = tokio::time::timeout(Duration::from_millis(100), socket.close()).await;
}

/// The clock is checked on every transport poll; timer readiness is only a
/// wakeup. This prevents delayed timers authorizing expired protected writes.
async fn bounded_write<F: Future<Output = bool>>(
    deadline: Instant,
    allowed: impl Fn() -> bool,
    write: F,
) -> bool {
    tokio::pin!(write);
    let checked = std::future::poll_fn(|cx| {
        if Instant::now() >= deadline || !allowed() {
            Poll::Ready(false)
        } else {
            write.as_mut().poll(cx)
        }
    });
    tokio::select! {biased;_=tokio::time::sleep_until(deadline.into())=>false,result=checked=>result}
}
async fn send(
    pilot: &Pilot,
    subscription: &crate::runtime::Subscription,
    role: Role,
    socket: &mut WebSocket,
    event: Value,
) -> bool {
    if !pilot.event_current(role, &event) {
        // Never start writing a queued generated fragment after Finish. A write
        // already pending when authority changes instead fails the poll fence
        // below and retires the socket, because a partial frame is uncertain.
        return pilot.connection_current(&subscription.id, &subscription.session, role);
    }
    let Ok(text) = serde_json::to_string(&event) else {
        return false;
    };
    if text.len() > 128 * 1024 {
        return false;
    }
    let deadline = pilot.operation_deadline(Duration::from_millis(400));
    let mut stopped = pilot.stop_receiver();
    let mut public_changed = pilot.public_receiver();
    let started = std::sync::atomic::AtomicBool::new(false);
    tokio::select! {biased;_=stopped.changed()=>false,_=public_changed.changed(),if role==Role::Public=>false,result=bounded_write(deadline,||pilot.connection_current(&subscription.id,&subscription.session,role)&&pilot.in_flight_event_current(role,&event,started.load(std::sync::atomic::Ordering::Acquire)),async{started.store(true,std::sync::atomic::Ordering::Release);socket.send(Message::Text(text.into())).await.is_ok()})=>result}
}

async fn index(State(s): State<HttpState>) -> Response {
    static_file(s.assets.join("index.html"), "text/html; charset=utf-8").await
}
async fn asset(State(s): State<HttpState>, Path(path): Path<String>) -> Response {
    if path.len() > 256
        || !path
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b"-_.".contains(&b))
        || path.contains("..")
    {
        return StatusCode::NOT_FOUND.into_response();
    }
    let kind = match FilePath::new(&path).extension().and_then(|e| e.to_str()) {
        Some("js") => "text/javascript",
        Some("css") => "text/css",
        Some("wasm") => "application/wasm",
        Some("svg") => "image/svg+xml",
        _ => return StatusCode::NOT_FOUND.into_response(),
    };
    static_file(s.assets.join("assets").join(path), kind).await
}
async fn static_file(path: PathBuf, kind: &'static str) -> Response {
    match tokio::task::spawn_blocking(move || {
        crate::config::read_file(&path, 8 * 1024 * 1024, false)
    })
    .await
    {
        Ok(Ok(bytes)) => ([(header::CONTENT_TYPE, kind)], bytes).into_response(),
        _ => StatusCode::NOT_FOUND.into_response(),
    }
}

#[cfg(test)]
mod write_tests {
    use super::bounded_write;
    use std::{
        cell::Cell,
        future::{Future, poll_fn},
        task::{Context, Poll, Waker},
        time::{Duration, Instant},
    };

    #[tokio::test]
    async fn expired_or_revoked_write_never_polls_transport() {
        for (deadline, allowed) in [
            (Instant::now(), true),
            (Instant::now() + Duration::from_secs(1), false),
        ] {
            let polls = Cell::new(0);
            let sent = bounded_write(
                deadline,
                || allowed,
                poll_fn(|_| {
                    polls.set(polls.get() + 1);
                    Poll::Ready(true)
                }),
            )
            .await;
            assert!(!sent);
            assert_eq!(polls.get(), 0);
        }
    }

    #[tokio::test]
    async fn revocation_between_transport_polls_prevents_resumed_write() {
        let allowed = Cell::new(true);
        let polls = Cell::new(0);
        let write = bounded_write(
            Instant::now() + Duration::from_secs(1),
            || allowed.get(),
            poll_fn(|_| {
                polls.set(polls.get() + 1);
                if polls.get() == 1 {
                    Poll::Pending
                } else {
                    Poll::Ready(true)
                }
            }),
        );
        tokio::pin!(write);
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(write.as_mut().poll(&mut cx), Poll::Pending);
        allowed.set(false);
        assert_eq!(write.as_mut().poll(&mut cx), Poll::Ready(false));
        assert_eq!(polls.get(), 1, "no transport poll after revocation");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn stalled_timer_cannot_authorize_expired_resumed_transport() {
        let polls = Cell::new(0);
        let deadline = Instant::now() + Duration::from_millis(100);
        let write = bounded_write(
            deadline,
            || true,
            poll_fn(|_| {
                polls.set(polls.get() + 1);
                if polls.get() == 1 {
                    Poll::Pending
                } else {
                    Poll::Ready(true)
                }
            }),
        );
        tokio::pin!(write);
        let mut cx = Context::from_waker(Waker::noop());
        assert_eq!(write.as_mut().poll(&mut cx), Poll::Pending);
        // Deliberately do not yield to the runtime's timer driver. The write
        // would become Ready on its next poll unless the wall clock fences it.
        std::thread::sleep(Duration::from_millis(110));
        assert!(Instant::now() >= deadline);
        assert_eq!(write.as_mut().poll(&mut cx), Poll::Ready(false));
        assert_eq!(polls.get(), 1, "no transport poll after lease expiry");
    }

    #[tokio::test]
    async fn current_write_can_complete_and_idle_write_times_out() {
        assert!(
            bounded_write(Instant::now() + Duration::from_secs(1), || true, async {
                true
            })
            .await
        );
        assert!(
            !bounded_write(
                Instant::now() + Duration::from_millis(10),
                || true,
                std::future::pending::<bool>(),
            )
            .await
        );
    }
}
