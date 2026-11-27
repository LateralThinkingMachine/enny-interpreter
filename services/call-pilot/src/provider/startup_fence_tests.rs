//! Startup admission is distinct from cancelling an existing source drain.
//! In-memory WebSockets and synthetic counters only; no provider connection.
use super::*;
use crate::engine::{DrainCompletion, StreamInput};
use futures_util::{SinkExt, StreamExt};
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use tokio::{
    io::DuplexStream,
    sync::{mpsc, oneshot},
};
use tokio_tungstenite::{WebSocketStream, tungstenite::Message};

#[tokio::test(flavor = "current_thread")]
async fn fence_after_registration_prevents_first_connect_poll() {
    let (fence, receiver) = watch::channel(false);
    let backend = OpenAiBackend::new_with_controls(config(), vec![], Some(receiver)).unwrap();
    let shared = backend.shared.clone();
    let polls = Arc::new(AtomicUsize::new(0));
    let attempted = polls.clone();
    let (done, result) = oneshot::channel();
    backend
        .shared
        .spawn_owned(async move {
            let (_local, mut local) = watch::channel(false);
            let mut global = shared.stop.subscribe();
            let result = guard::until_admitted(
                Instant::now() + Duration::from_secs(5),
                &mut local,
                &mut global,
                &shared.revocation,
                Some(&shared.admission),
                async move {
                    attempted.fetch_add(1, Ordering::SeqCst);
                },
            )
            .await;
            let _ = done.send(result);
        })
        .unwrap();
    // No await between registration and fence: the owned task has not run.
    fence.send_replace(true);
    assert!(result.await.unwrap().is_err());
    assert_eq!(polls.load(Ordering::SeqCst), 0);
    assert!(
        backend
            .shared
            .reserve(Some(Route::Transcription), 0)
            .is_err()
    );
    assert!(backend.shared.spawn_owned(async {}).is_err());
    assert_eq!(
        backend.budget_snapshot().stream_reservations.unwrap().used,
        0
    );
    backend.shutdown().await;
}

#[tokio::test(flavor = "current_thread")]
async fn admission_fence_repoll_refuses_a_previously_pending_startup_write() {
    let (fence, receiver) = watch::channel(false);
    let backend = OpenAiBackend::new_with_controls(config(), vec![], Some(receiver)).unwrap();
    let (_local, mut local) = watch::channel(false);
    let mut global = backend.shared.stop.subscribe();
    let polls = AtomicUsize::new(0);
    let ready = AtomicBool::new(false);
    let mut future = Box::pin(guard::until_admitted(
        Instant::now() + Duration::from_secs(30),
        &mut local,
        &mut global,
        &backend.shared.revocation,
        Some(&backend.shared.admission),
        std::future::poll_fn(|_| {
            polls.fetch_add(1, Ordering::SeqCst);
            if ready.load(Ordering::SeqCst) {
                Poll::Ready(())
            } else {
                Poll::Pending
            }
        }),
    ));
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);
    assert!(future.as_mut().poll(&mut cx).is_pending());
    fence.send_replace(true);
    ready.store(true, Ordering::SeqCst);
    assert!(matches!(
        future.as_mut().poll(&mut cx),
        Poll::Ready(Err(ProviderError::Deadline))
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    drop(future);
    assert!(!*local.borrow());
    assert!(!*global.borrow());
    fence.send_replace(false);
    assert!(backend.shared.admission.revoked());
}

struct Stream {
    server: WebSocketStream<DuplexStream>,
    input: mpsc::Sender<StreamInput>,
    output: mpsc::Receiver<StreamEvent>,
    _stop: watch::Sender<bool>,
    task: tokio::task::JoinHandle<Result<(), (bool, ProviderError)>>,
}
async fn stream(backend: &OpenAiBackend) -> Stream {
    let (mut client, server) = pair(65536).await;
    let (input, input_rx) = mpsc::channel(8);
    let (events, output) = mpsc::channel(32);
    let (stop, mut stopped) = watch::channel(false);
    let shared = backend.shared.clone();
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Transcription, Language::Ja);
        transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await
    });
    Stream {
        server,
        input,
        output,
        _stop: stop,
        task,
    }
}
async fn created(stream: &mut Stream) {
    stream
        .server
        .send(Message::Text(
            json!({"type":"session.created",
        "session":resolved(Route::Transcription)})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
    let startup = tokio::time::timeout(Duration::from_secs(1), stream.server.next())
        .await
        .unwrap()
        .unwrap()
        .unwrap();
    let Message::Text(text) = startup else {
        panic!("startup must be a text frame")
    };
    assert_eq!(
        serde_json::from_str::<Value>(&text).unwrap(),
        protocol::start(Route::Transcription, Language::Ja)
    );
}
async fn updated(stream: &mut Stream) {
    stream
        .server
        .send(Message::Text(
            json!({"type":"session.updated",
        "session":resolved(Route::Transcription)})
            .to_string()
            .into(),
        ))
        .await
        .unwrap();
}

#[tokio::test]
async fn fenced_session_updated_cannot_release_ready() {
    let (fence, receiver) = watch::channel(false);
    let backend = OpenAiBackend::new_with_controls(config(), vec![], Some(receiver)).unwrap();
    let mut s = stream(&backend).await;
    created(&mut s).await;
    fence.send_replace(true);
    updated(&mut s).await;
    let report = tokio::time::timeout(Duration::from_secs(1), s.task)
        .await
        .unwrap()
        .unwrap();
    assert!(report.is_ok() || matches!(report, Err((_, ProviderError::Deadline))));
    assert!(s.output.recv().await.is_none());
    assert!(!*backend.shared.stop.borrow());
}

#[tokio::test]
async fn finish_fence_preserves_admitted_source_drain_but_not_sibling_startup() {
    let (fence, receiver) = watch::channel(false);
    let backend = OpenAiBackend::new_with_controls(config(), vec![], Some(receiver)).unwrap();
    let mut admitted = stream(&backend).await;
    created(&mut admitted).await;
    updated(&mut admitted).await;
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), admitted.output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Ready { .. })
    ));
    let mut pending = stream(&backend).await;
    created(&mut pending).await;
    fence.send_replace(true);
    updated(&mut pending).await;
    admitted.input.send(StreamInput::FinishInput).await.unwrap();
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(3), admitted.output.recv())
            .await
            .unwrap(),
        Some(StreamEvent::Drained {
            completion: DrainCompletion::CommittedTranscriptsFinal
        })
    ));
    assert!(admitted.task.await.unwrap().is_ok());
    let report = tokio::time::timeout(Duration::from_secs(1), pending.task)
        .await
        .unwrap()
        .unwrap();
    assert!(report.is_ok() || matches!(report, Err((_, ProviderError::Deadline))));
    assert!(pending.output.recv().await.is_none());
    assert!(!*backend.shared.stop.borrow());
    assert!(!backend.enabled(), "a drained stream cannot admit new work");
}
