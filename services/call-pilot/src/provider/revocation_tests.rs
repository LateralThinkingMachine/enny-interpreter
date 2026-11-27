use super::*;
use crate::budget::BudgetState;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

#[tokio::test]
async fn already_revoked_parent_never_polls_connect_or_allocates() {
    let (parent, receiver) = watch::channel(false);
    let backend = OpenAiBackend::new_with_revocation(config(), vec![receiver]).unwrap();
    parent.send_replace(true);
    assert!(!backend.enabled());
    assert!(!backend.supports(Route::Live));
    assert!(backend.shared.reserve(Some(Route::Live), 0).is_err());
    let budget = backend.budget_snapshot();
    assert_eq!(budget.state, BudgetState::Stopped);
    assert!(budget.routes.iter().all(|route| !route.can_reserve));
    assert_eq!(budget.stream_reservations.unwrap().used, 0);
    let (_local, mut local) = watch::channel(false);
    let (_global, mut global) = watch::channel(false);
    let mut polled = false;
    assert!(
        guard::active(
            Instant::now() + Duration::from_secs(1),
            &mut local,
            &mut global,
            &backend.shared.revocation,
            async {
                polled = true;
            }
        )
        .await
        .is_err()
    );
    assert!(!polled);
    assert!(!*local.borrow());
    assert!(!*global.borrow());
}

#[tokio::test(flavor = "current_thread")]
async fn parent_stop_repoll_blocks_inner_write_without_supervisor_or_timer_wakeup() {
    let (parent, receiver) = watch::channel(false);
    let external = guard::ExternalRevocation::new(vec![receiver]).unwrap();
    let (_local, mut local) = watch::channel(false);
    let (_global, mut global) = watch::channel(false);
    let polls = AtomicUsize::new(0);
    let ready = AtomicBool::new(false);
    let write = std::future::poll_fn(|_| {
        polls.fetch_add(1, Ordering::SeqCst);
        if ready.load(Ordering::SeqCst) {
            Poll::Ready(())
        } else {
            Poll::Pending
        }
    });
    let mut future = Box::pin(guard::active(
        Instant::now() + Duration::from_secs(30),
        &mut local,
        &mut global,
        &external,
        write,
    ));
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);
    assert!(future.as_mut().poll(&mut cx).is_pending());
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    parent.send_replace(true);
    ready.store(true, Ordering::SeqCst);
    assert!(matches!(
        future.as_mut().poll(&mut cx),
        Poll::Ready(Err(ProviderError::Deadline))
    ));
    assert_eq!(polls.load(Ordering::SeqCst), 1);
    drop(future);
    assert!(!*local.borrow());
    assert!(!*global.borrow());
    parent.send_replace(false);
    assert!(external.revoked(), "observed parent Stop cannot be reset");
}

#[tokio::test]
async fn either_parent_wakes_a_blocked_guard_without_inner_transport_activity() {
    for selected in [0, 1] {
        let (first, first_rx) = watch::channel(false);
        let (second, second_rx) = watch::channel(false);
        let external = guard::ExternalRevocation::new(vec![first_rx, second_rx]).unwrap();
        let (_local, mut local) = watch::channel(false);
        let (_global, mut global) = watch::channel(false);
        let task = tokio::spawn(async move {
            guard::active(
                Instant::now() + Duration::from_secs(30),
                &mut local,
                &mut global,
                &external,
                std::future::pending::<()>(),
            )
            .await
        });
        tokio::task::yield_now().await;
        if selected == 0 {
            first.send_replace(true);
        } else {
            second.send_replace(true);
        }
        assert!(
            tokio::time::timeout(Duration::from_secs(1), task)
                .await
                .unwrap()
                .unwrap()
                .is_err()
        );
    }
}

#[test]
fn parent_closure_revokes_and_receiver_count_is_bounded() {
    let (sender, receiver) = watch::channel(false);
    assert!(OpenAiBackend::new_with_revocation(config(), vec![receiver.clone(); 3]).is_err());
    let backend = OpenAiBackend::new_with_revocation(config(), vec![receiver]).unwrap();
    assert!(backend.enabled());
    drop(sender);
    assert!(!backend.enabled());
    assert_eq!(backend.budget_snapshot().state, BudgetState::Stopped);
}

#[tokio::test]
async fn actual_stream_ready_and_generated_output_are_fenced_by_parent_stop() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::Message;
    let (parent, receiver) = watch::channel(false);
    let backend = OpenAiBackend::new_with_revocation(config(), vec![receiver]).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (_input, input) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut protocol = protocol::Protocol::new(Route::Translation, Language::Ja);
        transport::drive(
            &shared,
            &mut client,
            &mut protocol,
            input,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await
    });
    server
        .send(Message::Text(
            json!({"type":"session.created","session":resolved(Route::Translation)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let _startup = server.next().await.unwrap().unwrap();
    parent.send_replace(true);
    server
        .send(Message::Text(
            json!({"type":"session.updated","session":resolved(Route::Translation)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    let report = tokio::time::timeout(Duration::from_secs(1), task)
        .await
        .unwrap()
        .unwrap();
    assert!(report.is_ok() || matches!(report, Err((_, ProviderError::Deadline))));
    assert!(
        output.recv().await.is_none(),
        "no Ready or output after direct parent revocation"
    );
    assert!(
        !*backend.shared.stop.borrow(),
        "the inner stop channel was never copied from the parent"
    );
}

struct Dropped(Arc<AtomicBool>);
impl Drop for Dropped {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_shutdown_aborts_removed_owned_tasks_and_preserves_uncertainty() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let dropped = Arc::new(AtomicBool::new(false));
    let marker = Dropped(dropped.clone());
    backend
        .shared
        .spawn_owned(async move {
            let _marker = marker;
            std::future::pending::<()>().await;
        })
        .unwrap();
    tokio::task::yield_now().await;
    let mut shutdown = Box::pin(backend.shutdown());
    let waker = Waker::from(Arc::new(Noop));
    let mut cx = Context::from_waker(&waker);
    assert!(shutdown.as_mut().poll(&mut cx).is_pending());
    assert!(backend.shared.tasks.lock().unwrap().is_empty());
    drop(shutdown);
    tokio::time::timeout(Duration::from_secs(1), async {
        while !dropped.load(Ordering::SeqCst) {
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap();
    assert_eq!(backend.budget_snapshot().uncertain, Some(true));
    backend.shutdown().await;
    assert_eq!(backend.budget_snapshot().uncertain, Some(true));
    assert!(!backend.enabled());
}
