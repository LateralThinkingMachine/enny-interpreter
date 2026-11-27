use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::{
    Connector, connect_async_tls_with_config,
    tungstenite::{Message, client::IntoClientRequest, protocol::WebSocketConfig},
};

pub(super) struct Provider {
    key: String,
    client: reqwest::Client,
    base: String,
    socket_base: String,
}
fn tls() -> Result<rustls::ClientConfig, &'static str> {
    Ok(rustls::ClientConfig::builder_with_provider(Arc::new(
        rustls::crypto::ring::default_provider(),
    ))
    .with_safe_default_protocol_versions()
    .map_err(|_| "webrtc_tls")?
    .with_root_certificates(rustls::RootCertStore::from_iter(
        webpki_roots::TLS_SERVER_ROOTS.iter().cloned(),
    ))
    .with_no_client_auth())
}
impl Provider {
    pub(super) fn new(key: String) -> Result<Self, &'static str> {
        let client = reqwest::Client::builder()
            .use_preconfigured_tls(tls()?)
            .no_proxy()
            .redirect(reqwest::redirect::Policy::none())
            .https_only(true)
            .connect_timeout(Duration::from_secs(3))
            .timeout(Duration::from_secs(12))
            .build()
            .map_err(|_| "webrtc_http")?;
        Ok(Self {
            key,
            client,
            base: "https://api.openai.com/v1/live/sessions".into(),
            socket_base: "wss://api.openai.com/v1/live/sessions".into(),
        })
    }
    #[cfg(test)]
    pub(super) fn fixture(base: String, socket_base: String) -> Self {
        Self {
            key: "fixture-only-no-real-key".into(),
            client: reqwest::Client::builder()
                .use_preconfigured_tls(tls().unwrap())
                .no_proxy()
                .redirect(reqwest::redirect::Policy::none())
                .timeout(Duration::from_secs(2))
                .build()
                .unwrap(),
            base,
            socket_base,
        }
    }
    async fn create(&self, offer: &Offer) -> Result<Value, &'static str> {
        let mut response = self
            .client
            .post(&self.base)
            .bearer_auth(&self.key)
            .header("content-type", "application/json")
            .body(offer.provider_body().to_string())
            .send()
            .await
            .map_err(|_| "creation_unconfirmed")?;
        if response.status().is_client_error() {
            return Err("creation_rejected");
        }
        if response.status() != reqwest::StatusCode::CREATED {
            return Err("creation_unconfirmed");
        }
        let mut bytes = Vec::new();
        while let Some(chunk) = response.chunk().await.map_err(|_| "creation_unconfirmed")? {
            if bytes.len() + chunk.len() > 96 * 1024 {
                return Err("creation_unconfirmed");
            }
            bytes.extend_from_slice(&chunk);
        }
        let value: Value = serde_json::from_slice(&bytes).map_err(|_| "creation_unconfirmed")?;
        let id = value["session"]["id"]
            .as_str()
            .filter(|id| {
                !id.is_empty()
                    && id.len() <= 200
                    && id
                        .bytes()
                        .all(|c| c.is_ascii_alphanumeric() || c == b'_' || c == b'-')
            })
            .ok_or("creation_unconfirmed")?;
        // Once an ID is known, preserve it even if the answer is malformed so
        // the owner can attach and close the remotely created session.
        let transport = value["transport"]["sdp"]
            .as_str()
            .filter(|v| {
                v.starts_with("v=0\r\n")
                    && v.len() <= MAX_SDP
                    && !v.contains('\0')
                    && !v.contains("m=video ")
            })
            .filter(|_| value["transport"]["type"] == "webrtc")
            .map(|sdp| json!({"type":"webrtc","sdp":sdp}));
        Ok(json!({"session":{"id":id},"transport":transport}))
    }
    pub(super) async fn run(
        &self,
        owner: Arc<Ownership>,
        offer: Offer,
        send: oneshot::Sender<Result<Value, ApiError>>,
    ) {
        if !owner.current() {
            let _ = send.send(Err(ApiError::Stopped));
            owner.complete(true, Some("start_cancelled"));
            return;
        }
        // Do not cancel an in-flight creation POST: a stopped browser must not
        // orphan a session whose response could still be received and closed.
        let created = match self.create(&offer).await {
            Ok(value) => value,
            Err(issue) => {
                let _ = send.send(Err(ApiError::Unavailable));
                owner.complete(issue == "creation_rejected", Some(issue));
                return;
            }
        };
        let id = created["session"]["id"].as_str().unwrap().to_owned();
        if created["transport"].is_null() {
            owner.observation.lock().unwrap().issue = Some("invalid_provider_answer");
            owner.stop();
        }
        owner.observation.lock().unwrap().provider_id = Some(id.clone());
        let connect = async {
            let mut request = format!("{}/{}/attach", self.socket_base, id)
                .into_client_request()
                .map_err(|_| ())?;
            let mut header = format!("Bearer {}", self.key)
                .parse::<axum::http::HeaderValue>()
                .map_err(|_| ())?;
            header.set_sensitive(true);
            request.headers_mut().insert("Authorization", header);
            connect_async_tls_with_config(
                request,
                Some(
                    WebSocketConfig::default()
                        .max_message_size(Some(128 * 1024))
                        .max_frame_size(Some(128 * 1024)),
                ),
                true,
                Some(Connector::Rustls(Arc::new(tls().map_err(|_| ())?))),
            )
            .await
            .map(|(socket, _)| socket)
            .map_err(|_| ())
        };
        let mut socket = match tokio::time::timeout(Duration::from_secs(5), connect).await {
            Ok(Ok(s)) => s,
            _ => {
                let _ = send.send(Err(ApiError::Unavailable));
                owner.complete(false, Some("sideband_unavailable"));
                return;
            }
        };
        let mut answer = created;
        answer["generation"] = json!(offer.generation);
        answer["request_id"] = json!(offer.request_id);
        answer["requested_voice"] = json!(offer.voice);
        answer["duration_seconds"] = json!(DURATION_SECONDS);
        answer["ducking"] = ducking();
        if owner.current() {
            owner.observation.lock().unwrap().answer = Some(answer.clone());
            let _ = send.send(Ok(answer));
        } else {
            let _ = send.send(Err(ApiError::Stopped));
        }
        let mut tick = tokio::time::interval(Duration::from_millis(100));
        let mut stop = owner.cancel.subscribe();
        let mut pilot_stop = owner.pilot.stop_receiver();
        let mut closing = None;
        let mut usage = 0.0;
        let mut text_bytes = 0usize;
        let mut confirmed = false;
        let mut issue = None;
        loop {
            if !owner.current() && closing.is_none() {
                owner.stop();
                closing = Some(Instant::now());
                if !matches!(
                    tokio::time::timeout(
                        Duration::from_secs(2),
                        socket.send(Message::Text(
                            json!({"type":"session.close"}).to_string().into()
                        ))
                    )
                    .await,
                    Ok(Ok(()))
                ) {
                    break;
                }
            }
            if closing.is_some_and(|at: Instant| at.elapsed() > Duration::from_secs(10)) {
                issue = Some("closure_timeout");
                break;
            }
            tokio::select! { biased;
                _=tick.tick()=>{},
                _=stop.changed(),if closing.is_none()=>{},
                _=pilot_stop.changed(),if closing.is_none()=>{},
                event=socket.next()=>match event {
                    Some(Ok(Message::Text(text)))=> {
                        let Ok(value)=serde_json::from_str::<Value>(&text) else {issue=Some("sideband_protocol");owner.stop();continue;};
                        match value["type"].as_str() {
                            Some("session.started")=> {
                                if value["session"]["id"]!=id || value["session"]["model"]!="gpt-live-1" {issue=Some("sideband_identity");owner.stop();continue;}
                                if closing.is_none() {owner.observation.lock().unwrap().phase="connected";}
                            },
                            Some("session.usage.updated"|"session.closed")=> {
                                let seconds=value["usage"]["seconds"].as_f64();
                                if (value["type"]=="session.closed" && (value["session"]["id"]!=id || value["session"]["model"]!="gpt-live-1"))
                                    || seconds.is_none_or(|v|!v.is_finite()||v<usage||v>(DURATION_SECONDS+35)as f64) {
                                    issue=Some("usage_unconfirmed");owner.stop();continue;
                                }
                                usage=seconds.unwrap();owner.observation.lock().unwrap().usage_seconds=Some(usage);
                                if value["type"]=="session.closed" {
                                    confirmed=true;owner.observation.lock().unwrap().usage_confirmed=true;break;
                                }
                            },
                            Some("error")=>{issue=Some("provider_error");owner.stop();},
                            Some("session.input_transcript.delta"|"session.output_transcript.delta")=> {
                                text_bytes=text_bytes.saturating_add(text.len());
                                if text_bytes>76800*4 {issue=Some("text_capacity");owner.stop();}
                            },
                            Some("session.delegation.created")=>{issue=Some("delegation_unavailable");owner.stop();},
                            _=>{}
                        }
                    },
                    Some(Ok(Message::Ping(_)|Message::Pong(_)))=>{},
                    _=>break
                }
            }
        }
        let _ = tokio::time::timeout(Duration::from_millis(200), socket.close(None)).await;
        owner.complete(
            confirmed,
            issue.or(if confirmed {
                None
            } else {
                Some("sideband_disconnected")
            }),
        );
    }
}
