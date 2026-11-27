use super::*;
use axum::{
    body::Body,
    http::{HeaderValue, Request, StatusCode},
};
use http_body_util::BodyExt;
use tower::ServiceExt;

struct WindowBackend(Mutex<Option<StreamHandle>>);
impl ProviderBackend for WindowBackend {
    fn enabled(&self) -> bool {
        true
    }
    fn open(&self, _: Route, _: Language) -> crate::engine::ProviderFuture<StreamHandle> {
        let handle = self.0.lock().unwrap().take();
        Box::pin(async move { handle.ok_or(crate::engine::ProviderError::Unavailable) })
    }
    fn synthesize(
        &self,
        _: String,
        _: Language,
    ) -> crate::engine::ProviderFuture<crate::engine::PreparedAudio> {
        Box::pin(async { Err(crate::engine::ProviderError::Disabled) })
    }
}

struct WindowFixture {
    pilot: Arc<Pilot>,
    headers: HeaderMap,
    subscription: Subscription,
    events: mpsc::Sender<StreamEvent>,
    input: mpsc::Receiver<StreamInput>,
    route_id: String,
    renewal_fence: watch::Receiver<bool>,
}

impl WindowFixture {
    async fn new(continuous: bool) -> Self {
        Self::with_route(continuous, Route::Transcription).await
    }

    async fn with_route(continuous: bool, route: Route) -> Self {
        let (input, input_rx) = mpsc::channel(8);
        let (events, output) = mpsc::channel(32);
        let (renewal_fence, fence_receiver) = watch::channel(false);
        let backend = Arc::new(WindowBackend(Mutex::new(Some(StreamHandle {
            renewal_fence: continuous.then_some(renewal_fence),
            input,
            events: output,
            stop: watch::channel(false).0,
        }))));
        let (pilot, ticket) = if continuous {
            Pilot::new_persistent(backend, 1, auth::LocalOrigins::default()).unwrap()
        } else {
            Pilot::new(backend, Duration::from_secs(60), 1).unwrap()
        };
        let headers = login(&pilot, &ticket);
        let subscription = pilot
            .subscribe(
                pilot.authorize(&headers, Role::Operator, false).unwrap(),
                Role::Operator,
            )
            .unwrap();
        let route_id = pilot
            .start_route(1, Source::RemoteMix, route, Language::Ja)
            .unwrap();
        let mut fixture = Self {
            pilot,
            headers,
            subscription,
            events,
            input: input_rx,
            route_id,
            renewal_fence: fence_receiver,
        };
        assert_eq!(fixture.next().await["state"], "starting");
        fixture.ready().await;
        fixture
    }

    async fn next(&mut self) -> Value {
        tokio::time::timeout(Duration::from_secs(1), self.subscription.events.recv())
            .await
            .unwrap()
            .unwrap()
    }

    async fn ready(&mut self) {
        self.events
            .send(StreamEvent::Ready {
                expires_in_seconds: 50,
            })
            .await
            .unwrap();
        assert_eq!(self.next().await["state"], "ready");
        if self.pilot.is_continuous() {
            let window = self.next().await;
            assert_eq!(window["type"], "window");
            assert_eq!(window["state"], "ready");
        }
    }

    async fn handover(&mut self) {
        self.events.send(StreamEvent::Handover).await.unwrap();
        let notice = self.next().await;
        assert_eq!(notice["type"], "window");
        assert_eq!(notice["state"], "draining");
    }

    async fn source(&mut self, final_fragment: bool) -> Value {
        self.source_revision(final_fragment, 1).await
    }

    async fn source_revision(&mut self, final_fragment: bool, revision: u32) -> Value {
        self.events
            .send(StreamEvent::Transcript {
                provenance: crate::engine::Provenance::SourceAsr,
                text: "架空の会話".into(),
                final_fragment,
                item: Some(crate::engine::TranscriptItem {
                    source_audio: None,
                    id: "provider_reused_item_id".into(),
                    sequence: 1,
                    revision,
                }),
            })
            .await
            .unwrap();
        self.next().await
    }
}

fn persistent() -> (Arc<Pilot>, String) {
    Pilot::new_persistent(crate::engine::disabled(), 1, auth::LocalOrigins::default()).unwrap()
}

fn login(pilot: &Pilot, ticket: &str) -> HeaderMap {
    let (session, token) = pilot.redeem(ticket, Role::Operator).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert(
        "authorization",
        HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
    );
    headers.insert(
        "x-pilot-csrf",
        HeaderValue::from_str(session["csrf"].as_str().unwrap()).unwrap(),
    );
    headers
}

fn frame(sequence: u32) -> Vec<u8> {
    let mut bytes = vec![0; 646];
    bytes[0] = 1;
    bytes[2..6].copy_from_slice(&sequence.to_le_bytes());
    bytes
}

#[test]
fn local_workspace_has_no_process_timer_but_stop_and_detach_remain_terminal() {
    let (mut pilot, ticket) = persistent();
    Arc::get_mut(&mut pilot).unwrap().started_at = Instant::now() - Duration::from_secs(10_800);
    let headers = login(&pilot, &ticket);
    assert!(pilot.deadline().is_none());
    assert!(pilot.is_continuous());
    assert!(!pilot.is_stopped());
    let session = pilot.session(&headers, Role::Operator).unwrap();
    assert_eq!(session["continuous"], true);
    assert!(session["expires_in_seconds"].is_null());
    assert_eq!(session["providers_enabled"], false);
    assert_eq!(session["capabilities"]["source_captions"], false);
    let budget = pilot.budget(&headers).unwrap();
    assert_eq!(budget.state, crate::budget::BudgetState::Disabled);
    assert!(budget.remaining_seconds.is_none());
    let subscribed = pilot
        .subscribe(
            pilot.authorize(&headers, Role::Operator, false).unwrap(),
            Role::Operator,
        )
        .unwrap();
    pilot.disconnected(&subscribed.id, Role::Operator);
    let stopped = pilot.diagnostics(&headers).unwrap();
    assert!(stopped.stopped);
    let stop = stopped.stop.unwrap();
    assert_eq!(stop.code, StopCode::OperatorDisconnected);
    assert!(stop.elapsed_seconds >= 10_800);
    assert!(pilot.is_stopped());
    assert!(matches!(
        pilot.positive(pilot.generation()),
        Err(ApiError::Stopped)
    ));
    assert!(matches!(
        pilot.subscribe(subscribed.session, Role::Operator),
        Err(ApiError::Stopped)
    ));
    assert!(matches!(
        pilot.public_ticket(pilot.generation()),
        Err(ApiError::Stopped)
    ));
    assert!(pilot.session(&headers, Role::Operator).unwrap()["stopped"] == true);
}

#[test]
fn bounded_operations_and_one_use_tickets_do_not_inherit_persistent_lifetime() {
    let (pilot, ticket) = persistent();
    let before = Instant::now();
    let deadline = pilot.operation_deadline(Duration::from_millis(40));
    assert!(deadline >= before + Duration::from_millis(40));
    assert!(deadline <= Instant::now() + Duration::from_millis(40));
    let headers = login(&pilot, &ticket);
    assert!(matches!(
        pilot.redeem(&ticket, Role::Operator),
        Err(ApiError::Unauthorized)
    ));
    let public = pilot.public_ticket(pilot.generation()).unwrap();
    assert_eq!(public["expires_in_seconds"], 120);
    let public_value = public["ticket"].as_str().unwrap();
    pilot.memory.lock().unwrap().public_ticket = Some(Ticket::new(
        public_value,
        Instant::now() - Duration::from_millis(1),
    ));
    assert!(matches!(
        pilot.redeem(public_value, Role::Public),
        Err(ApiError::Unauthorized)
    ));
    assert!(pilot.authorize(&headers, Role::Operator, true).is_ok());
    assert!(matches!(
        pilot.authorize(&headers, Role::Public, false),
        Err(ApiError::Unauthorized)
    ));
}

#[test]
fn legacy_local_duration_and_frame_limit_remain_finite() {
    let (mut pilot, ticket) =
        Pilot::new(crate::engine::disabled(), Duration::from_secs(60), 1).unwrap();
    let headers = login(&pilot, &ticket);
    assert!(!pilot.is_continuous());
    assert!(pilot.deadline().is_some());
    assert_eq!(
        pilot.session(&headers, Role::Operator).unwrap()["continuous"],
        false
    );
    assert!(
        pilot
            .budget(&headers)
            .unwrap()
            .remaining_seconds
            .is_some_and(|remaining| remaining > 0 && remaining <= 60)
    );
    pilot.memory.lock().unwrap().frames = 50;
    assert!(matches!(pilot.frame(&frame(0)), Err(ApiError::Stopped)));
    assert_eq!(
        pilot
            .diagnostics(&headers)
            .unwrap()
            .last_issue
            .unwrap()
            .code,
        IssueCode::InputAudioLimit
    );
    Arc::get_mut(&mut pilot).unwrap().deadline = Some(Instant::now());
    assert!(matches!(
        pilot.authorize(&headers, Role::Operator, false),
        Err(ApiError::Unauthorized)
    ));
}

#[test]
fn persistent_input_keeps_pacing_sequence_and_checked_counter_guards() {
    let (pilot, ticket) = persistent();
    let headers = login(&pilot, &ticket);
    // Crossing the old aggregate frame threshold cannot expire local control.
    pilot.memory.lock().unwrap().frames = 14_400 * 50;
    pilot.frame(&frame(0)).unwrap();
    assert!(!pilot.is_stopped());
    assert_eq!(pilot.memory.lock().unwrap().frames, 14_400 * 50 + 1);
    assert!(matches!(pilot.frame(&frame(2)), Err(ApiError::Stopped)));
    assert_eq!(
        pilot
            .diagnostics(&headers)
            .unwrap()
            .last_issue
            .unwrap()
            .code,
        IssueCode::SequenceGap
    );

    let (pilot, ticket) = persistent();
    let headers = login(&pilot, &ticket);
    {
        let mut memory = pilot.memory.lock().unwrap();
        memory.sources[0].allowance = 0.0;
        // Freeze token accrual for this synthetic guard check; a loaded test
        // machine must not accidentally earn another 20ms frame allowance.
        memory.sources[0].updated = Instant::now() + Duration::from_secs(1);
    }
    assert!(matches!(pilot.frame(&frame(0)), Err(ApiError::Stopped)));
    assert_eq!(
        pilot
            .diagnostics(&headers)
            .unwrap()
            .last_issue
            .unwrap()
            .code,
        IssueCode::Backpressure
    );

    for sequence_overflow in [false, true] {
        let (pilot, ticket) = persistent();
        let headers = login(&pilot, &ticket);
        let sequence = if sequence_overflow {
            pilot.memory.lock().unwrap().sources[0].sequence = u32::MAX;
            u32::MAX
        } else {
            pilot.memory.lock().unwrap().frames = u64::MAX;
            0
        };
        assert!(matches!(
            pilot.frame(&frame(sequence)),
            Err(ApiError::Stopped)
        ));
        assert_eq!(
            pilot
                .diagnostics(&headers)
                .unwrap()
                .last_issue
                .unwrap()
                .code,
            IssueCode::InputAudioLimit
        );
    }
}

#[tokio::test]
async fn persistent_login_issues_an_explicit_credential_and_retains_role_origin_boundaries() {
    let (pilot, ticket) = persistent();
    let app = crate::router(
        pilot.clone(),
        std::path::PathBuf::from("/nonexistent-synthetic-assets"),
    );
    let response = app
        .clone()
        .oneshot(
            Request::builder()
                .method("POST")
                .uri("/api/bootstrap")
                .header("host", auth::HOST)
                .header("origin", auth::ORIGIN)
                .header("content-type", "application/json")
                .body(Body::from(json!({"ticket":ticket}).to_string()))
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(!response.headers().contains_key("set-cookie"));
    let token = response.headers()["x-pilot-session"].to_str().unwrap();
    let authorization = format!("Bearer {token}");
    let body: Value =
        serde_json::from_slice(&response.into_body().collect().await.unwrap().to_bytes()).unwrap();
    assert_eq!(body["continuous"], true);
    assert!(body["expires_in_seconds"].is_null());
    let public = crate::public_router(
        pilot.clone(),
        std::path::PathBuf::from("/nonexistent-synthetic-assets"),
    );
    let response = public
        .oneshot(
            Request::builder()
                .uri("/api/diagnostics")
                .header("host", auth::PUBLIC_HOST)
                .header("authorization", &authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let response = app
        .oneshot(
            Request::builder()
                .uri("/api/session")
                .header("host", "127.0.0.1:5080")
                .header("authorization", &authorization)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
}

#[tokio::test]
async fn confirmed_window_boundary_namespaces_source_items_and_keeps_outer_input_flowing() {
    let mut fixture = WindowFixture::new(true).await;
    let first = fixture.source(true).await;
    assert_eq!(first["window"], 1);
    fixture.handover().await;
    fixture
        .events
        .send(StreamEvent::Renewing { window: 2 })
        .await
        .unwrap();
    let renewal = fixture.next().await;
    assert_eq!(renewal["type"], "window");
    assert_eq!(renewal["state"], "renewing");
    assert_eq!(renewal["window"], 2);
    assert_eq!(renewal["route_id"], fixture.route_id);
    {
        let memory = fixture.pilot.memory.lock().unwrap();
        let route = &memory.routes[&fixture.route_id];
        assert!(route.renewing && !route.ready);
    }
    fixture.pilot.frame(&frame(0)).unwrap();
    assert!(
        matches!(fixture.input.recv().await, Some(StreamInput::Pcm16(bytes)) if bytes.len() == 640)
    );
    fixture.ready().await;
    let next = fixture.source(true).await;
    assert_eq!(next["window"], 2);
    assert_eq!(next["item"], first["item"]);
    assert!(!fixture.pilot.is_stopped());
    fixture.pilot.stop("operator_stop");
    assert!(fixture.pilot.is_stopped());
    assert!(matches!(
        fixture.pilot.frame(&frame(1)),
        Err(ApiError::Stopped)
    ));
}

#[tokio::test]
async fn renewal_cannot_hide_unsealed_source_items_skip_windows_or_change_legacy_scope() {
    for (continuous, unsealed, next) in [(true, true, 2), (true, false, 3), (false, false, 2)] {
        let mut fixture = WindowFixture::new(continuous).await;
        if unsealed {
            let source = fixture.source(false).await;
            assert_eq!(source["final"], false);
        }
        if continuous {
            fixture.handover().await;
        }
        fixture
            .events
            .send(StreamEvent::Renewing { window: next })
            .await
            .unwrap();
        let gap = fixture.next().await;
        assert_eq!(gap["type"], "gap");
        assert_eq!(gap["code"], "provider_protocol");
        assert!(fixture.pilot.memory.lock().unwrap().routes.is_empty());
        assert_eq!(
            fixture
                .pilot
                .diagnostics(&fixture.headers)
                .unwrap()
                .last_issue
                .unwrap()
                .code,
            IssueCode::ProviderProtocol
        );
        fixture.pilot.stop("operator_stop");
    }
}

#[tokio::test]
async fn output_cannot_cross_a_window_boundary_before_the_new_provider_is_ready() {
    let mut fixture = WindowFixture::new(true).await;
    fixture.source(true).await;
    fixture.handover().await;
    fixture
        .events
        .send(StreamEvent::Renewing { window: 2 })
        .await
        .unwrap();
    assert_eq!(fixture.next().await["state"], "renewing");
    let rejected = fixture.source(true).await;
    assert_eq!(rejected["type"], "gap");
    assert_eq!(rejected["code"], "provider_protocol");
    assert!(fixture.pilot.memory.lock().unwrap().routes.is_empty());
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn generated_audio_is_namespaced_by_window_without_any_playback() {
    let mut fixture = WindowFixture::with_route(true, Route::Live).await;
    for window in 1..=2 {
        fixture
            .events
            .send(StreamEvent::Audio {
                pcm16: vec![0; 640],
                sample_rate: 16000,
            })
            .await
            .unwrap();
        let audio = fixture.next().await;
        assert_eq!(audio["type"], "audio");
        assert_eq!(audio["window"], window);
        if window == 1 {
            fixture.handover().await;
            fixture
                .events
                .send(StreamEvent::Renewing { window: 2 })
                .await
                .unwrap();
            assert_eq!(fixture.next().await["state"], "renewing");
            fixture.ready().await;
        }
    }
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn handover_notice_preserves_old_source_finals_until_the_confirmed_boundary() {
    let mut fixture = WindowFixture::new(true).await;
    let draft = fixture.source(false).await;
    assert_eq!(draft["window"], 1);
    fixture.handover().await;
    let final_item = fixture.source_revision(true, 2).await;
    assert_eq!(final_item["type"], "transcript");
    assert_eq!(final_item["window"], 1);
    assert_eq!(final_item["final"], true);
    fixture
        .events
        .send(StreamEvent::Renewing { window: 2 })
        .await
        .unwrap();
    assert_eq!(fixture.next().await["state"], "renewing");
    fixture.ready().await;
    assert_eq!(fixture.source(true).await["window"], 2);
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn handover_is_required_once_and_internal_input_sealed_cannot_reach_the_ui() {
    for (continuous, prior_handover, event) in [
        (false, false, StreamEvent::Handover),
        (true, true, StreamEvent::Handover),
        (true, false, StreamEvent::Renewing { window: 2 }),
        (true, false, StreamEvent::InputSealed),
        (false, false, StreamEvent::InputSealed),
    ] {
        let mut fixture = WindowFixture::new(continuous).await;
        if prior_handover {
            fixture.handover().await;
        }
        fixture.events.send(event).await.unwrap();
        let gap = fixture.next().await;
        assert_eq!(gap["type"], "gap");
        assert_eq!(gap["code"], "provider_protocol");
        assert!(fixture.pilot.memory.lock().unwrap().routes.is_empty());
        fixture.pilot.stop("operator_stop");
    }
}

#[tokio::test]
async fn finish_adopts_a_valid_handover_already_queued_before_its_fence() {
    let mut fixture = WindowFixture::new(true).await;
    fixture.source(false).await;
    // No yield between enqueue and Finish: the consumer sees this old-window
    // notice only after the synchronous Finish fence has been latched.
    fixture.events.try_send(StreamEvent::Handover).unwrap();
    fixture
        .pilot
        .finish_from_socket(1, &fixture.subscription.id, &fixture.subscription.session)
        .unwrap();
    assert!(*fixture.renewal_fence.borrow());
    assert_eq!(fixture.next().await["type"], "finishing");
    let draining = fixture.next().await;
    assert_eq!(draining["state"], "draining");
    assert_eq!(draining["window"], 1);
    assert!(matches!(
        fixture.input.recv().await,
        Some(StreamInput::FinishInput)
    ));
    let final_item = fixture.source_revision(true, 2).await;
    assert_eq!(final_item["type"], "transcript");
    assert_eq!(final_item["window"], 1);
    fixture
        .events
        .send(StreamEvent::Drained {
            completion: crate::engine::DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    fixture
        .events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert_eq!(fixture.next().await["state"], "closed");
    let finished = fixture.next().await;
    assert_eq!(finished["type"], "finished");
    assert_eq!(finished["source_completion"], "complete");
    assert!(!fixture.pilot.is_stopped());
    assert!(
        fixture
            .pilot
            .diagnostics(&fixture.headers)
            .unwrap()
            .last_issue
            .is_none()
    );
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn finish_racing_with_queued_renewal_preserves_old_final_and_reports_incomplete() {
    let mut fixture = WindowFixture::new(true).await;
    let final_item = fixture.source(true).await;
    assert_eq!(final_item["window"], 1);
    fixture.handover().await;
    fixture
        .events
        .try_send(StreamEvent::Renewing { window: 2 })
        .unwrap();
    fixture
        .events
        .try_send(StreamEvent::Ready {
            expires_in_seconds: 50,
        })
        .unwrap();
    fixture
        .pilot
        .finish_from_socket(1, &fixture.subscription.id, &fixture.subscription.session)
        .unwrap();
    assert!(*fixture.renewal_fence.borrow());
    assert_eq!(fixture.next().await["type"], "finishing");
    let gap = fixture.next().await;
    assert_eq!(gap["type"], "gap");
    assert_eq!(gap["code"], "source_incomplete");
    let finished = fixture.next().await;
    assert_eq!(finished["type"], "finished");
    assert_eq!(finished["source_completion"], "incomplete");
    assert!(fixture.subscription.events.try_recv().is_err());
    assert!(fixture.pilot.memory.lock().unwrap().routes.is_empty());
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn finish_fences_new_provider_windows_before_the_full_source_queue_can_drain() {
    let mut fixture = WindowFixture::new(true).await;
    for sequence in 0..8 {
        fixture.pilot.frame(&frame(sequence)).unwrap();
    }
    assert_eq!(fixture.input.len(), 8);
    assert!(!*fixture.renewal_fence.borrow());
    fixture
        .pilot
        .finish_from_socket(1, &fixture.subscription.id, &fixture.subscription.session)
        .unwrap();
    // This assertion is synchronous: the producer's source queue remains full,
    // so its ordered FinishInput cannot yet have reached the supervisor.
    assert!(*fixture.renewal_fence.borrow());
    assert_eq!(fixture.input.len(), 8);
    assert!(fixture.pilot.is_finishing());
    assert!(!fixture.pilot.is_stopped());
    for _ in 0..8 {
        assert!(matches!(
            fixture.input.recv().await,
            Some(StreamInput::Pcm16(_))
        ));
    }
    assert!(matches!(
        tokio::time::timeout(Duration::from_secs(1), fixture.input.recv())
            .await
            .unwrap(),
        Some(StreamInput::FinishInput)
    ));
    assert!(*fixture.renewal_fence.borrow());
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn recovery_namespaces_unfinished_source_and_finish_still_reports_interrupted_coverage() {
    let mut fixture = WindowFixture::new(true).await;
    let partial = fixture.source(false).await;
    assert_eq!(partial["window"], 1);
    fixture
        .events
        .send(StreamEvent::Recovering {
            window: 2,
            error: crate::engine::ProviderError::ConnectionLost,
            retry_in_seconds: 1,
        })
        .await
        .unwrap();
    let recovery = fixture.next().await;
    assert_eq!(recovery["state"], "recovering");
    assert_eq!(recovery["code"], "provider_connection_lost");
    assert_eq!(recovery["retry_in_seconds"], 1);
    assert_eq!(recovery["window"], 2);
    assert_eq!(recovery["route_id"], fixture.route_id);
    assert!(!fixture.pilot.memory.lock().unwrap().sources[0].closed);
    assert!(fixture.pilot.memory.lock().unwrap().sources[0].interrupted);
    fixture.pilot.frame(&frame(0)).unwrap();
    assert!(matches!(
        fixture.input.recv().await,
        Some(StreamInput::Pcm16(_))
    ));
    fixture.ready().await;
    let new = fixture.source(true).await;
    assert_eq!(new["window"], 2);
    assert_eq!(new["item"], partial["item"]);
    fixture
        .pilot
        .finish_from_socket(1, &fixture.subscription.id, &fixture.subscription.session)
        .unwrap();
    assert_eq!(fixture.next().await["type"], "finishing");
    assert!(matches!(
        fixture.input.recv().await,
        Some(StreamInput::FinishInput)
    ));
    fixture
        .events
        .send(StreamEvent::Drained {
            completion: crate::engine::DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    fixture
        .events
        .send(StreamEvent::Closed { confirmed: true })
        .await
        .unwrap();
    assert_eq!(fixture.next().await["state"], "closed");
    let completed = fixture.next().await;
    assert_eq!(completed["type"], "finished");
    assert_eq!(completed["source_completion"], "incomplete");
    fixture.pilot.stop("operator_stop");
}

#[tokio::test]
async fn recovery_rejects_finite_routes_fatal_codes_invalid_backoff_and_window_skips() {
    use crate::engine::ProviderError;
    for (continuous, next, error, delay) in [
        (false, 2, ProviderError::ConnectionLost, 1),
        (true, 3, ProviderError::ConnectionLost, 1),
        (true, 2, ProviderError::UsageMismatch, 1),
        (true, 2, ProviderError::ConnectionLost, 31),
    ] {
        let mut fixture = WindowFixture::new(continuous).await;
        fixture
            .events
            .send(StreamEvent::Recovering {
                window: next,
                error,
                retry_in_seconds: delay,
            })
            .await
            .unwrap();
        let failure = fixture.next().await;
        assert_eq!(failure["type"], "gap");
        assert_eq!(failure["code"], "provider_protocol");
        assert!(fixture.pilot.memory.lock().unwrap().routes.is_empty());
        fixture.pilot.stop("operator_stop");
    }
}

#[test]
fn continuous_operator_lease_tolerates_timer_jitter_but_cannot_revive_expired_connection() {
    for continuous in [false, true] {
        let (pilot, ticket) = if continuous {
            persistent()
        } else {
            Pilot::new(crate::engine::disabled(), Duration::from_secs(60), 1).unwrap()
        };
        let headers = login(&pilot, &ticket);
        let session = pilot.authorize(&headers, Role::Operator, false).unwrap();
        let subscription = pilot.subscribe(session.clone(), Role::Operator).unwrap();
        pilot
            .memory
            .lock()
            .unwrap()
            .operator_connection
            .as_mut()
            .unwrap()
            .heartbeat = Instant::now() - Duration::from_secs(6);
        assert_eq!(
            pilot.connection_current(&subscription.id, &session, Role::Operator),
            continuous
        );
        assert_eq!(
            pilot.heartbeat(&subscription.id, Role::Operator, 1).is_ok(),
            continuous
        );
        pilot
            .memory
            .lock()
            .unwrap()
            .operator_connection
            .as_mut()
            .unwrap()
            .heartbeat = Instant::now() - Duration::from_secs(31);
        assert!(!pilot.connection_current(&subscription.id, &session, Role::Operator));
        assert!(
            pilot
                .heartbeat(&subscription.id, Role::Operator, 1)
                .is_err()
        );
        assert_eq!(pilot.heartbeat_lease(Role::Public), Duration::from_secs(5));
        pilot.stop("operator_stop");
    }
}

fn draft(revision: u32, text: &str, retired: bool) -> StreamEvent {
    StreamEvent::SourceDraft {
        item_id: "provider_reused_item_id".into(),
        revision,
        text: text.into(),
        retired,
    }
}

#[tokio::test]
async fn provisional_source_drafts_are_private_and_separate_from_canonical_items() {
    for continuous in [false, true] {
        let mut fixture = WindowFixture::new(continuous).await;
        let ticket = fixture.pilot.public_ticket(1).unwrap();
        let (viewer, token) = fixture
            .pilot
            .redeem(ticket["ticket"].as_str().unwrap(), Role::Public)
            .unwrap();
        let mut viewer_headers = HeaderMap::new();
        viewer_headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let mut public = fixture
            .pilot
            .subscribe(
                fixture
                    .pilot
                    .authorize(&viewer_headers, Role::Public, false)
                    .unwrap(),
                Role::Public,
            )
            .unwrap();
        let _ = viewer;
        fixture
            .events
            .send(draft(1, "金額は", false))
            .await
            .unwrap();
        let event = fixture.next().await;
        assert_eq!(event["type"], "source_draft");
        assert_eq!(event["route_id"], fixture.route_id);
        assert_eq!(event["source"], "remote_mix");
        assert_eq!(event["route"], "transcription");
        assert_eq!(event["text"], "金額は");
        assert_eq!(event["revision"], 1);
        assert_eq!(event["retired"], false);
        assert!(
            event.get("item").is_none()
                && event.get("final").is_none()
                && event.get("provenance").is_none()
        );
        assert_eq!(event.get("window"), continuous.then_some(&json!(1)));
        fixture
            .events
            .send(draft(2, "金額は12,000円", false))
            .await
            .unwrap();
        assert_eq!(fixture.next().await["revision"], 2);
        fixture.events.send(draft(2, "", true)).await.unwrap();
        assert_eq!(fixture.next().await["retired"], true);
        let canonical = fixture.source(true).await;
        assert_eq!(canonical["type"], "transcript");
        assert_eq!(canonical["item"]["sequence"], 1);
        assert!(public.events.try_recv().is_err());
        assert!(!fixture.pilot.is_stopped());
        fixture.pilot.stop("operator_stop");
    }
}

#[tokio::test]
async fn provisional_draft_identity_cannot_bypass_readiness_receipts_or_route_type() {
    for mode in [
        "unready",
        "generated_route",
        "bind_early",
        "already_bound",
        "bad_revision",
        "replaced_snapshot",
    ] {
        let mut fixture = WindowFixture::with_route(
            true,
            if mode == "generated_route" {
                Route::Translation
            } else {
                Route::Transcription
            },
        )
        .await;
        if mode == "unready" {
            fixture
                .pilot
                .memory
                .lock()
                .unwrap()
                .routes
                .get_mut(&fixture.route_id)
                .unwrap()
                .ready = false;
        }
        if mode == "already_bound" {
            fixture.source(true).await;
        }
        if ["bind_early", "bad_revision", "replaced_snapshot"].contains(&mode) {
            fixture
                .events
                .send(draft(1, "unbound", false))
                .await
                .unwrap();
            assert_eq!(fixture.next().await["type"], "source_draft");
        }
        match mode {
            "bind_early" => {
                fixture
                    .events
                    .send(StreamEvent::Transcript {
                        provenance: crate::engine::Provenance::SourceAsr,
                        text: "canonical without retirement".into(),
                        final_fragment: true,
                        item: Some(crate::engine::TranscriptItem {
                            source_audio: None,
                            id: "provider_reused_item_id".into(),
                            sequence: 1,
                            revision: 1,
                        }),
                    })
                    .await
                    .unwrap();
            }
            "bad_revision" => fixture
                .events
                .send(draft(3, "unbound next", false))
                .await
                .unwrap(),
            "replaced_snapshot" => fixture
                .events
                .send(draft(2, "different prefix", false))
                .await
                .unwrap(),
            _ => fixture
                .events
                .send(draft(1, "unbound", false))
                .await
                .unwrap(),
        }
        let gap = fixture.next().await;
        assert_eq!(gap["type"], "gap", "{mode}");
        assert_eq!(gap["code"], "provider_protocol");
        assert!(fixture.pilot.memory.lock().unwrap().routes.is_empty());
        fixture.pilot.stop("operator_stop");
    }
}

#[tokio::test]
async fn recovery_discards_provisional_window_state_but_normal_renewal_requires_binding() {
    let mut fixture = WindowFixture::new(true).await;
    fixture
        .events
        .send(draft(1, "old unbound text", false))
        .await
        .unwrap();
    assert_eq!(fixture.next().await["window"], 1);
    fixture
        .events
        .send(StreamEvent::Recovering {
            window: 2,
            error: ProviderError::ConnectionLost,
            retry_in_seconds: 1,
        })
        .await
        .unwrap();
    assert_eq!(fixture.next().await["state"], "recovering");
    fixture.ready().await;
    fixture
        .events
        .send(draft(1, "new window text", false))
        .await
        .unwrap();
    let next = fixture.next().await;
    assert_eq!(next["window"], 2);
    assert_eq!(next["revision"], 1);
    assert!(fixture.pilot.memory.lock().unwrap().sources[0].interrupted);
    fixture.pilot.stop("operator_stop");
    for retire_without_binding in [false, true] {
        let mut fixture = WindowFixture::new(true).await;
        fixture
            .events
            .send(draft(1, "not a source item", false))
            .await
            .unwrap();
        fixture.next().await;
        if retire_without_binding {
            fixture.events.send(draft(1, "", true)).await.unwrap();
            fixture.next().await;
        }
        fixture.handover().await;
        fixture
            .events
            .send(StreamEvent::Renewing { window: 2 })
            .await
            .unwrap();
        assert_eq!(fixture.next().await["code"], "provider_protocol");
        fixture.pilot.stop("operator_stop");
    }
}

#[tokio::test]
async fn unresolved_draft_cannot_complete_finish_and_stop_never_emits_queued_drafts() {
    let mut fixture = WindowFixture::new(true).await;
    fixture
        .events
        .send(draft(1, "unbound at finish", false))
        .await
        .unwrap();
    fixture.next().await;
    fixture
        .pilot
        .finish_from_socket(1, &fixture.subscription.id, &fixture.subscription.session)
        .unwrap();
    assert_eq!(fixture.next().await["type"], "finishing");
    assert!(matches!(
        fixture.input.recv().await,
        Some(StreamInput::FinishInput)
    ));
    fixture
        .events
        .send(StreamEvent::Drained {
            completion: crate::engine::DrainCompletion::CommittedTranscriptsFinal,
        })
        .await
        .unwrap();
    assert_eq!(fixture.next().await["code"], "provider_protocol");
    fixture.pilot.stop("operator_stop");
    let mut fixture = WindowFixture::new(true).await;
    fixture
        .events
        .try_send(draft(1, "queued before Stop", false))
        .unwrap();
    fixture.pilot.stop("operator_stop");
    assert_eq!(fixture.next().await["type"], "stopped");
    tokio::task::yield_now().await;
    assert!(fixture.subscription.events.try_recv().is_err());
}

#[tokio::test]
async fn retired_manual_pause_does_not_expire_idle_control_but_disconnect_is_sticky() {
    let (pilot, ticket) = Pilot::new_hosted(
        crate::engine::disabled(),
        None,
        600,
        auth::LocalOrigins::default(),
        1,
        true,
        Instant::now(),
    )
    .unwrap();
    let (view, token) = pilot.redeem(&ticket, Role::Operator).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    headers.insert(
        "x-pilot-csrf",
        view["csrf"].as_str().unwrap().parse().unwrap(),
    );
    let identity = pilot.authorize(&headers, Role::Operator, false).unwrap();
    let sub = pilot.subscribe(identity, Role::Operator).unwrap();
    pilot.pause_processing(1).unwrap();
    pilot
        .memory
        .lock()
        .unwrap()
        .operator_connection
        .as_mut()
        .unwrap()
        .heartbeat = Instant::now() - Duration::from_secs(3600);
    assert!(!pilot.connection_current(&sub.id, &sub.session, Role::Operator));
    pilot.mark_pause_retired(true);
    assert!(pilot.connection_current(&sub.id, &sub.session, Role::Operator));
    assert!(pilot.heartbeat(&sub.id, Role::Operator, 1).is_ok());
    pilot.disconnected(&sub.id, Role::Operator);
    assert!(pilot.is_stopped());
    assert!(!pilot.connection_current(&sub.id, &sub.session, Role::Operator));
}
