//! Actual ephemeral loopback transport; only the provider is an in-memory mock.
use super::*;
use futures_util::{SinkExt, StreamExt};
use tokio::net::{TcpListener, TcpStream};
use tokio_tungstenite::{
    WebSocketStream,
    tungstenite::{Message, client::IntoClientRequest},
};

struct Running(tokio::task::JoinHandle<()>);
impl Drop for Running {
    fn drop(&mut self) {
        self.0.abort();
    }
}

async fn next(socket: &mut WebSocketStream<TcpStream>) -> Value {
    for _ in 0..8 {
        let value = tokio::time::timeout(Duration::from_secs(2), socket.next())
            .await
            .unwrap()
            .unwrap()
            .unwrap();
        if let Message::Text(text) = value {
            let event: Value = serde_json::from_str(&text).unwrap();
            if event["type"] != "heartbeat" {
                return event;
            }
        }
    }
    panic!("No bounded expected protocol event");
}

#[tokio::test]
async fn actual_same_socket_pcm_finish_barrier_preserves_last_final_and_closes_owned_listener() {
    let _ = rustls::crypto::ring::default_provider().install_default();
    let backend = ChannelBackend::new(true);
    let (pilot, ticket) = Pilot::new(backend.clone(), Duration::from_secs(60), 30).unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let app = crate::router(
        pilot.clone(),
        std::path::PathBuf::from("/private/tmp/no-pilot-assets"),
    );
    let server = Running(tokio::spawn(async move {
        axum::serve(listener, app).await.unwrap();
    }));
    tokio::time::timeout(Duration::from_secs(8),async {
        let client = reqwest::Client::builder().no_proxy().redirect(reqwest::redirect::Policy::none())
            .timeout(Duration::from_secs(2)).build().unwrap();
        let response = client.post(format!("http://{address}/api/bootstrap"))
            .header("host",auth::HOST).header("origin",auth::ORIGIN)
            .header("content-type","application/json").body(json!({"ticket":ticket}).to_string()).send().await.unwrap();
        assert_eq!(response.status(),reqwest::StatusCode::OK);
        let token = response.headers()["x-pilot-session"].to_str().unwrap().to_owned();
        let session: Value = serde_json::from_slice(&response.bytes().await.unwrap()).unwrap();
        let mut request = format!("ws://{address}/api/operator/events").into_client_request().unwrap();
        request.headers_mut().insert("host",auth::HOST.parse().unwrap());
        request.headers_mut().insert("origin",auth::ORIGIN.parse().unwrap());
        request.headers_mut().insert("sec-websocket-protocol",format!("pilot.events.v1, pilot.auth.{token}").parse().unwrap());
        let (mut socket,_) = tokio_tungstenite::client_async(request,TcpStream::connect(address).await.unwrap()).await.unwrap();
        assert_eq!(next(&mut socket).await["type"],"hello");
        let response = client.post(format!("http://{address}/api/routes"))
            .header("host",auth::HOST).header("origin",auth::ORIGIN).bearer_auth(&token)
            .header("x-pilot-csrf",session["csrf"].as_str().unwrap())
            .header("content-type","application/json").body(json!({"generation":1,"source":"remote_mix","route":"transcription","language":"ja"}).to_string())
            .send().await.unwrap();
        assert_eq!(response.status(),reqwest::StatusCode::ACCEPTED);
        assert_eq!(next(&mut socket).await["state"],"starting");
        assert_eq!(next(&mut socket).await["state"],"ready");
        // Do not await any application acknowledgement between frames and
        // Finish: actual WebSocket ordering supplies the capture-end barrier.
        for sequence in 0..8 { socket.send(Message::Binary(frame(sequence).into())).await.unwrap(); }
        socket.send(Message::Text(json!({"type":"finish","generation":1}).to_string().into())).await.unwrap();
        assert_eq!(next(&mut socket).await,json!({"type":"finishing","generation":1}));
        let mut input = backend.inputs.lock().unwrap().remove(0);
        for sequence in 0..8 {
            let Some(StreamInput::Pcm16(bytes)) = input.recv().await else { panic!("Lost pre-Finish PCM frame"); };
            assert_eq!(bytes,vec![sequence as u8;640]);
        }
        assert!(matches!(input.recv().await,Some(StreamInput::FinishInput)));
        let events = backend.events.lock().unwrap()[0].clone();
        events.send(final_item()).await.unwrap();
        events.send(StreamEvent::Drained{completion:DrainCompletion::CommittedTranscriptsFinal}).await.unwrap();
        events.send(StreamEvent::Closed{confirmed:true}).await.unwrap();
        let final_event = next(&mut socket).await;
        assert_eq!(final_event["type"],"transcript"); assert_eq!(final_event["text"],"The last accepted phrase.");
        assert_eq!(next(&mut socket).await["state"],"closed");
        assert_eq!(next(&mut socket).await,json!({"type":"finished","generation":1,"source_completion":"complete"}));
        assert!(!pilot.is_stopped());
        assert!(pilot.positive(1).is_err());
        socket.send(Message::Text(json!({"type":"heartbeat","generation":1}).to_string().into())).await.unwrap();
        socket.close(None).await.unwrap();
        while !pilot.is_stopped() { tokio::task::yield_now().await; }
        assert!(input.recv().await.is_none());
    }).await.unwrap();
    // Abort/join only this owned server and verify its socket is retired.
    server.0.abort();
    while !server.0.is_finished() {
        tokio::task::yield_now().await;
    }
    assert!(TcpStream::connect(address).await.is_err());
}
