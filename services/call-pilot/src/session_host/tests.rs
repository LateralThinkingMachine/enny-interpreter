use super::*;
use axum::{
    body::{Body, to_bytes},
    http::{Request, StatusCode},
};
use tower::ServiceExt;

pub(super) struct RetirementGate {
    pub entered: tokio::sync::Semaphore,
    pub release: tokio::sync::Semaphore,
    pub success: bool,
}
fn setup(repeatable: bool) -> (Arc<SessionHost>, Arc<Pilot>, HeaderMap) {
    let (host, ticket) = SessionHost::new(
        None,
        None,
        600,
        auth::LocalOrigins::default(),
        PathBuf::from("/nonexistent-test-assets"),
        repeatable,
    )
    .unwrap();
    let pilot = host.pilot();
    let (view, token) = pilot.redeem(&ticket, Role::Operator).unwrap();
    (host, pilot, headers(&token, view["csrf"].as_str().unwrap()))
}
fn headers(token: &str, csrf: &str) -> HeaderMap {
    let mut h = HeaderMap::new();
    h.insert("authorization", format!("Bearer {token}").parse().unwrap());
    h.insert("x-pilot-csrf", csrf.parse().unwrap());
    h
}
fn nonce(value: char) -> String {
    value.to_string().repeat(64)
}
fn gate(host: &SessionHost, success: bool) -> Arc<RetirementGate> {
    let gate = Arc::new(RetirementGate {
        entered: tokio::sync::Semaphore::new(0),
        release: tokio::sync::Semaphore::new(0),
        success,
    });
    *host.snapshot().retirement_gate.lock().unwrap() = Some(gate.clone());
    gate
}
fn spawn_new(
    host: &Arc<SessionHost>,
    pilot: &Arc<Pilot>,
    h: &HeaderMap,
) -> JoinHandle<Result<Arc<Receipt>, ApiError>> {
    let (host, pilot, h) = (host.clone(), pilot.clone(), h.clone());
    tokio::spawn(async move { host.new_session(&pilot, h, 1, nonce('a')).await })
}
async fn request(
    host: &Arc<SessionHost>,
    role: Role,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", auth::LocalOrigins::default().host(role))
        .header("origin", auth::LocalOrigins::default().origin(role))
        .header("content-type", "application/json");
    for (name, value) in headers {
        request = request.header(name, value);
    }
    let body = if method == "GET" && body.is_null() {
        Body::empty()
    } else {
        Body::from(body.to_string())
    };
    let response = crate::http::hosted_router(host.clone(), role)
        .oneshot(request.body(body).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = to_bytes(response.into_body(), 32 * 1024).await.unwrap();
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

#[tokio::test]
async fn stop_receipt_is_stopped_only_private_and_rotates_with_the_pinned_session() {
    let (host, old, h) = setup(true);
    let path = "/api/stop-receipt";
    let public_ticket = old.public_ticket(1).unwrap();
    let (viewer, token) = old
        .redeem(public_ticket["ticket"].as_str().unwrap(), Role::Public)
        .unwrap();
    let viewer = headers(&token, viewer["csrf"].as_str().unwrap());
    assert_eq!(
        request(&host, Role::Operator, "GET", path, &h, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    assert_eq!(
        request(&host, Role::Operator, "POST", "/api/stop", &h, json!({}))
            .await
            .0,
        StatusCode::OK
    );
    let (status, headers_out, receipt) =
        request(&host, Role::Operator, "GET", path, &h, Value::Null).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers_out["cache-control"], "no-store");
    assert_eq!(
        receipt,
        json!({"version":1,"session_generation":1,"generation":2,
        "provider_activity":"none_admitted","reserved_microusd":0,"invoice":"unavailable"})
    );
    assert_eq!(
        request(&host, Role::Public, "GET", path, &viewer, Value::Null)
            .await
            .0,
        StatusCode::FORBIDDEN
    );
    for denied in [viewer, HeaderMap::new()] {
        assert_eq!(
            request(&host, Role::Operator, "GET", path, &denied, Value::Null)
                .await
                .0,
            StatusCode::UNAUTHORIZED
        );
    }
    let next = host
        .new_session(&old, h.clone(), 1, nonce('c'))
        .await
        .unwrap();
    let current = headers(&next.token, next.view["csrf"].as_str().unwrap());
    assert_eq!(
        request(&host, Role::Operator, "GET", path, &h, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(&host, Role::Operator, "GET", path, &current, Value::Null)
            .await
            .0,
        StatusCode::CONFLICT
    );
    host.pilot().stop("operator_stop");
    let next = request(&host, Role::Operator, "GET", path, &current, Value::Null)
        .await
        .2;
    assert_eq!(
        (
            next["session_generation"].as_u64(),
            next["generation"].as_u64()
        ),
        (Some(3), Some(4))
    );
    host.pilot().logout(Role::Operator);
    assert_eq!(
        request(&host, Role::Operator, "GET", path, &current, Value::Null)
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn stopped_rollover_rotates_authority_and_preserves_sticky_predecessor() {
    let (host, old, h) = setup(true);
    assert!(matches!(
        host.new_session(&old, h.clone(), 1, nonce('a')).await,
        Err(ApiError::Conflict)
    ));
    old.stop("operator_stop");
    let stopped = old.session(&h, Role::Operator).unwrap();
    assert_eq!(stopped["generation"], 2);
    assert_eq!(stopped["session_generation"], 1);
    let receipt = host
        .new_session(&old, h.clone(), 1, nonce('a'))
        .await
        .unwrap();
    let fresh = host.pilot();
    assert!(old.is_stopped());
    assert!(!fresh.is_stopped());
    assert_eq!(receipt.view["generation"], 3);
    assert_eq!(receipt.view["session_generation"], 3);
    assert_eq!(receipt.view["repeatable_sessions"], true);
    assert_eq!(receipt.view["providers_enabled"], false);
    assert_ne!(receipt.view["csrf"], stopped["csrf"]);
    assert!(fresh.authorize(&h, Role::Operator, false).is_err());
    let new_h = headers(&receipt.token, receipt.view["csrf"].as_str().unwrap());
    assert!(fresh.authorize(&new_h, Role::Operator, true).is_ok());
    old.stop("operator_stop");
    assert!(!fresh.is_stopped());
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn new_session_diagnostics_keep_the_hosts_process_time_origin() {
    let (mut host, old, h) = setup(true);
    Arc::get_mut(&mut host).unwrap().process_started = Instant::now() - Duration::from_secs(120);
    old.stop("operator_stop");
    let receipt = host.new_session(&old, h, 1, nonce('a')).await.unwrap();
    let fresh = host.pilot();
    fresh.stop("operator_stop");
    let h = headers(&receipt.token, receipt.view["csrf"].as_str().unwrap());
    let diagnostic = fresh.diagnostics(&h).unwrap();
    assert!((120..=121).contains(&diagnostic.stop.unwrap().elapsed_seconds));
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn idempotent_receipt_accepts_only_exact_old_or_rotated_authority() {
    let (host, old, h) = setup(true);
    old.stop("operator_stop");
    let first = host
        .new_session(&old, h.clone(), 1, nonce('a'))
        .await
        .unwrap();
    let current = host.pilot();
    let replay = host
        .new_session(&current, h.clone(), 1, nonce('a'))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&first, &replay));
    let rotated_old_csrf = headers(&first.token, auth::single(&h, "x-pilot-csrf").unwrap());
    assert!(Arc::ptr_eq(
        &first,
        &host
            .new_session(&current, rotated_old_csrf.clone(), 1, nonce('a'))
            .await
            .unwrap()
    ));
    assert!(
        host.new_session(&current, rotated_old_csrf.clone(), 1, nonce('b'))
            .await
            .is_err()
    );
    assert!(
        host.new_session(&current, headers(&first.token, &nonce('f')), 1, nonce('a'))
            .await
            .is_err()
    );
    assert!(
        host.new_session(&current, h.clone(), 2, nonce('a'))
            .await
            .is_err()
    );
    current.stop("operator_stop");
    assert!(
        host.new_session(&current, rotated_old_csrf, 1, nonce('a'))
            .await
            .is_err()
    );
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn http_fences_old_mutations_and_public_surface_after_rollover() {
    let (host, old, h) = setup(true);
    let before = request(&host, Role::Operator, "GET", "/api/session", &h, json!({})).await;
    assert_eq!(before.0, StatusCode::OK);
    assert_eq!(before.2["session_generation"], 1);
    assert_eq!(before.2["generation"], 1);
    let public_ticket = old.public_ticket(1).unwrap();
    let (public_view, _) = old
        .redeem(public_ticket["ticket"].as_str().unwrap(), Role::Public)
        .unwrap();
    assert_eq!(public_view["repeatable_sessions"], false);
    assert!(public_view.get("session_generation").is_none());
    old.stop("operator_stop");
    let reloaded = request(&host, Role::Operator, "GET", "/api/session", &h, json!({})).await;
    assert_eq!(reloaded.0, StatusCode::OK);
    assert_eq!(reloaded.2["generation"], 2);
    assert_eq!(reloaded.2["csrf"], before.2["csrf"]);
    let body =
        json!({"previous_generation":reloaded.2["session_generation"],"request_id":nonce('a')});
    let (status, response_h, reply) = request(
        &host,
        Role::Operator,
        "POST",
        "/api/session/new",
        &h,
        body.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(reply["session"]["generation"], 3);
    assert!(!response_h.contains_key("set-cookie"));
    let new_token = response_h["x-pilot-session"].to_str().unwrap();
    let stale = headers(new_token, auth::single(&h, "x-pilot-csrf").unwrap());
    assert_eq!(
        request(
            &host,
            Role::Operator,
            "POST",
            "/api/stop",
            &stale,
            json!({})
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&host, Role::Operator, "GET", "/api/session", &h, json!({}))
            .await
            .0,
        StatusCode::UNAUTHORIZED
    );
    assert_eq!(
        request(
            &host,
            Role::Public,
            "POST",
            "/api/session/new",
            &h,
            body.clone()
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    assert_eq!(
        request(&host, Role::Operator, "POST", "/api/session/new", &h, body)
            .await
            .0,
        StatusCode::OK
    );
    assert!(!host.pilot().is_stopped());
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn pending_rollover_is_single_flight_and_http_cancellation_does_not_cancel_cleanup() {
    let (host, old, h) = setup(true);
    let gate = gate(&host, true);
    old.stop("operator_stop");
    let first = spawn_new(&host, &old, &h);
    gate.entered.acquire().await.unwrap().forget();
    let second = spawn_new(&host, &old, &h);
    assert!(matches!(
        host.new_session(&old, h.clone(), 1, nonce('b')).await,
        Err(ApiError::Conflict)
    ));
    first.abort();
    let _ = first.await;
    assert!(Arc::ptr_eq(&old, &host.pilot()));
    gate.release.add_permits(1);
    let result = second.await.unwrap().unwrap();
    assert_eq!(result.view["generation"], 3);
    assert!(Arc::ptr_eq(
        &result,
        &host
            .new_session(&host.pilot(), h, 1, nonce('a'))
            .await
            .unwrap()
    ));
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn logout_during_retirement_prevents_publication() {
    let (host, old, h) = setup(true);
    let gate = gate(&host, true);
    old.stop("operator_stop");
    let task = spawn_new(&host, &old, &h);
    gate.entered.acquire().await.unwrap().forget();
    host.logout(&old, &h).unwrap();
    gate.release.add_permits(1);
    assert!(matches!(task.await.unwrap(), Err(ApiError::Unauthorized)));
    assert!(Arc::ptr_eq(&old, &host.pilot()));
    *host.snapshot().retirement_gate.lock().unwrap() = None;
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn logout_retires_both_sides_before_during_and_after_credential_handoff() {
    for phase in [
        "pinned predecessor",
        "lost reply",
        "headers only",
        "successor",
    ] {
        let (host, old, h) = setup(true);
        old.stop("operator_stop");
        let receipt = host
            .new_session(&old, h.clone(), 1, nonce('a'))
            .await
            .unwrap();
        let current = host.pilot();
        if phase == "pinned predecessor" {
            host.logout(&old, &h).unwrap();
        } else {
            let proof = match phase {
                "lost reply" => h.clone(),
                "headers only" => {
                    headers(&receipt.token, auth::single(&h, "x-pilot-csrf").unwrap())
                }
                "successor" => headers(&receipt.token, receipt.view["csrf"].as_str().unwrap()),
                _ => unreachable!(),
            };
            let (status, response, _) = request(
                &host,
                Role::Operator,
                "POST",
                "/api/logout",
                &proof,
                json!({}),
            )
            .await;
            assert_eq!(status, StatusCode::OK);
            assert!(!response.contains_key("x-pilot-session"));
            assert!(!response.contains_key("set-cookie"));
        }
        assert!(host.state.lock().unwrap().receipt.is_none());
        assert!(old.authorize(&h, Role::Operator, true).is_err());
        assert!(current.is_stopped());
        assert!(
            current
                .authorize(
                    &headers(&receipt.token, receipt.view["csrf"].as_str().unwrap()),
                    Role::Operator,
                    false
                )
                .is_err()
        );
        assert!(host.state.lock().unwrap().receipt.is_none());
        assert!(host.shutdown().await);
    }
}

#[tokio::test]
async fn process_shutdown_joins_pending_retirement_and_prevents_successor() {
    let (host, old, h) = setup(true);
    let gate = gate(&host, true);
    old.stop("operator_stop");
    let task = spawn_new(&host, &old, &h);
    gate.entered.acquire().await.unwrap().forget();
    let shutting = {
        let host = host.clone();
        tokio::spawn(async move { host.shutdown().await })
    };
    tokio::task::yield_now().await;
    // Both rollover and final host shutdown retire the same predecessor.
    gate.release.add_permits(2);
    assert!(task.await.unwrap().is_err());
    assert!(shutting.await.unwrap());
    assert!(Arc::ptr_eq(&old, &host.pilot()));
    assert!(host.new_session(&old, h, 1, nonce('b')).await.is_err());
}

#[tokio::test]
async fn uncertain_local_retirement_cannot_publish_a_successor() {
    let (host, old, h) = setup(true);
    let gate = gate(&host, false);
    old.stop("operator_stop");
    let task = spawn_new(&host, &old, &h);
    gate.entered.acquire().await.unwrap().forget();
    gate.release.add_permits(1);
    assert!(matches!(task.await.unwrap(), Err(ApiError::Unavailable)));
    assert!(Arc::ptr_eq(&old, &host.pilot()));
    *host.snapshot().retirement_gate.lock().unwrap() = None;
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn finite_unconfigured_and_invalid_nonce_never_grant_rollover() {
    assert!(matches!(
        SessionHost::new(
            None,
            Some(60),
            600,
            auth::LocalOrigins::default(),
            PathBuf::new(),
            true
        ),
        Err("repeatable_scope_mismatch")
    ));
    let (host, old, h) = setup(false);
    old.stop("operator_stop");
    assert!(matches!(
        host.new_session(&old, h.clone(), 1, nonce('a')).await,
        Err(ApiError::Forbidden)
    ));
    assert_eq!(
        old.session(&h, Role::Operator).unwrap()["repeatable_sessions"],
        false
    );
    assert!(matches!(
        host.new_session(&old, h, 1, nonce('G')).await,
        Err(ApiError::Invalid)
    ));
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn repeated_sessions_replace_one_receipt_and_reject_all_earlier_authority() {
    let (host, initial, initial_headers) = setup(true);
    let (mut pilot, mut h) = (initial.clone(), initial_headers.clone());
    for index in 0..100_u64 {
        let stable = 1 + index * 2;
        pilot.stop("operator_stop");
        let receipt = host
            .new_session(&pilot, h, stable, format!("{index:064x}"))
            .await
            .unwrap();
        assert_eq!(receipt.view["generation"], stable + 2);
        pilot = host.pilot();
        h = headers(&receipt.token, receipt.view["csrf"].as_str().unwrap());
        assert!(host.state.lock().unwrap().pending.is_none());
    }
    assert_eq!(pilot.generation(), 201);
    assert!(
        host.new_session(&initial, initial_headers, 1, format!("{:064x}", 0))
            .await
            .is_err()
    );
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn expired_receipt_never_authenticates_old_credential_or_csrf() {
    let (host, old, h) = setup(true);
    old.stop("operator_stop");
    let receipt = host
        .new_session(&old, h.clone(), 1, nonce('a'))
        .await
        .unwrap();
    let rotated_old_csrf = headers(&receipt.token, auth::single(&h, "x-pilot-csrf").unwrap());
    drop(receipt);
    {
        let mut state = host.state.lock().unwrap();
        Arc::get_mut(state.receipt.as_mut().unwrap())
            .unwrap()
            .deadline = Instant::now() - Duration::from_secs(1);
    }
    assert!(host.logout(&host.pilot(), &rotated_old_csrf).is_err());
    assert!(host.logout(&host.pilot(), &h).is_err());
    assert!(
        host.new_session(&host.pilot(), h, 1, nonce('a'))
            .await
            .is_err()
    );
    assert!(host.state.lock().unwrap().receipt.is_none());
    assert!(!host.pilot().is_stopped());
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn one_use_continuous_provider_is_not_repeatable_and_constructor_opens_nothing() {
    let activation: ProviderActivation = serde_json::from_value(json!({"api_key":"fictional-never-transmitted", "continuous": {
        "route_duration_seconds":300,"max_concurrent_sessions":2,"max_input_audio_seconds":600,
        "max_output_audio_seconds":600,"max_text_characters":76800,"warning_threshold_microusd":5_000_000,
        "allowed_routes":["transcription","live"],"allowed_live_modes":["interpretation"]
    }})).unwrap();
    let (host, ticket) = SessionHost::new(
        Some(activation),
        None,
        600,
        auth::LocalOrigins::default(),
        PathBuf::new(),
        false,
    )
    .unwrap();
    let old = host.pilot();
    let (view, token) = old.redeem(&ticket, Role::Operator).unwrap();
    assert_eq!(view["repeatable_sessions"], false);
    assert!(view.get("session_generation").is_none());
    let h = headers(&token, view["csrf"].as_str().unwrap());
    old.stop("operator_stop");
    let receipt = old.stop_receipt(&h).unwrap();
    assert_eq!(receipt["provider_activity"], "unconfirmed");
    assert!(receipt["reserved_microusd"].is_null());
    assert!(matches!(
        host.new_session(&old, h, 1, nonce('a')).await,
        Err(ApiError::Forbidden)
    ));
    assert!(host.shutdown().await);
}

async fn await_pause(host: &Arc<SessionHost>, old: &Arc<Pilot>, h: &HeaderMap) -> Value {
    tokio::time::timeout(Duration::from_secs(2), async {
        loop {
            let status = serde_json::to_value(host.interpretation_state(old, h).unwrap()).unwrap();
            if status["state"] != "pausing" {
                return status;
            }
            tokio::task::yield_now().await;
        }
    })
    .await
    .unwrap()
}

#[tokio::test]
async fn manual_pause_fences_immediately_and_resume_rotates_without_unlatching_predecessor() {
    let (host, old, h) = setup(true);
    let identity = old.authorize(&h, Role::Operator, false).unwrap();
    let subscription = old.subscribe(identity, Role::Operator).unwrap();
    let retirement = gate(&host, true);
    let (status, _, body) = request(
        &host,
        Role::Operator,
        "POST",
        "/api/interpretation/pause",
        &h,
        json!({"generation":1}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["state"], "pausing");
    retirement.entered.acquire().await.unwrap().forget();
    assert!(old.is_manual_paused());
    assert!(!old.is_stopped());
    assert_eq!(old.generation(), 1);
    assert!(old.positive(1).is_err());
    assert!(old.finish(1).is_err());
    assert!(old.frame(&[0; 646]).is_err());
    assert!(old.heartbeat(&subscription.id, Role::Operator, 1).is_ok());
    assert!(old.event_current(Role::Operator, &json!({"type":"heartbeat"})));
    for event in [
        "audio",
        "transcript",
        "live_transcript",
        "source_draft",
        "route",
        "window",
        "preview",
    ] {
        assert!(!old.event_current(Role::Operator, &json!({"type":event})));
    }
    assert!(
        host.resume_interpretation(&old, h.clone(), 1, nonce('a'))
            .await
            .is_err()
    );
    assert!(
        host.new_session(&old, h.clone(), 1, nonce('a'))
            .await
            .is_err()
    );
    // Retry does not create a second retirement; dropping the HTTP response
    // never cancels the owned cleanup operation.
    host.pause_interpretation(&old, &h, 1).unwrap();
    assert_eq!(retirement.entered.available_permits(), 0);
    retirement.release.add_permits(1);
    let status = await_pause(&host, &old, &h).await;
    assert_eq!(status["state"], "paused");
    assert_eq!(status["local_retired"], true);
    assert_eq!(status["provider_closed_confirmed"], true);
    assert_eq!(status["resume_allowed"], true);
    let receipt = host
        .resume_interpretation(&old, h.clone(), 1, nonce('a'))
        .await
        .unwrap();
    let fresh = host.pilot();
    assert!(!Arc::ptr_eq(&fresh, &old));
    assert_eq!(receipt.view["generation"], 2);
    assert!(old.is_manual_paused());
    assert!(!fresh.is_manual_paused());
    assert!(!fresh.is_stopped());
    assert!(old.positive(1).is_err());
    let replay = host
        .resume_interpretation(&fresh, h.clone(), 1, nonce('a'))
        .await
        .unwrap();
    assert!(Arc::ptr_eq(&replay, &receipt));
    assert!(
        host.new_session(&fresh, h.clone(), 1, nonce('a'))
            .await
            .is_err()
    );
    assert!(
        host.resume_interpretation(&fresh, h.clone(), 1, nonce('b'))
            .await
            .is_err()
    );
    // No routes, devices or provider calls are opened by authority replacement.
    assert_eq!(
        fresh
            .session(
                &headers(&receipt.token, receipt.view["csrf"].as_str().unwrap()),
                Role::Operator
            )
            .unwrap()["generation"],
        2
    );
    assert!(host.shutdown().await);
}

#[tokio::test]
async fn pause_refuses_one_use_stale_generation_and_public_authority() {
    let (finite, old, h) = setup(false);
    assert!(finite.pause_interpretation(&old, &h, 1).is_err());
    assert!(!old.is_manual_paused());
    let (host, old, h) = setup(true);
    assert!(host.pause_interpretation(&old, &h, 2).is_err());
    assert!(!old.is_manual_paused());
    let mut missing_csrf = h.clone();
    missing_csrf.remove("x-pilot-csrf");
    for headers in [HeaderMap::new(), missing_csrf] {
        assert!(host.pause_interpretation(&old, &headers, 1).is_err());
    }
    let (status, _, _) = request(
        &host,
        Role::Public,
        "POST",
        "/api/interpretation/pause",
        &h,
        json!({"generation":1}),
    )
    .await;
    assert_ne!(status, StatusCode::ACCEPTED);
    assert!(!old.is_manual_paused());
    host.shutdown().await;
    finite.shutdown().await;
}

#[tokio::test]
async fn unconfirmed_retirement_never_admits_resume() {
    let (host, old, h) = setup(true);
    let retirement = gate(&host, false);
    host.pause_interpretation(&old, &h, 1).unwrap();
    retirement.entered.acquire().await.unwrap().forget();
    retirement.release.add_permits(1);
    let status = await_pause(&host, &old, &h).await;
    assert_eq!(status["state"], "closure_unconfirmed");
    assert_eq!(status["local_retired"], false);
    assert_eq!(status["provider_closed_confirmed"], false);
    assert_eq!(status["resume_allowed"], false);
    assert!(
        host.resume_interpretation(&old, h, 1, nonce('a'))
            .await
            .is_err()
    );
    // Remove test barrier so ordinary shutdown can join the already-retired owner.
    *host.snapshot().retirement_gate.lock().unwrap() = None;
    host.shutdown().await;
}

#[tokio::test]
async fn stop_and_logout_dominate_pending_pause_and_resume() {
    for logout in [false, true] {
        let (host, old, h) = setup(true);
        let retirement = gate(&host, true);
        host.pause_interpretation(&old, &h, 1).unwrap();
        retirement.entered.acquire().await.unwrap().forget();
        if logout {
            host.logout(&old, &h).unwrap();
        } else {
            old.stop("operator_stop");
        }
        retirement.release.add_permits(1);
        tokio::task::yield_now().await;
        assert!(
            host.resume_interpretation(&old, h.clone(), 1, nonce('a'))
                .await
                .is_err()
        );
        assert!(old.is_stopped());
        *host.snapshot().retirement_gate.lock().unwrap() = None;
        host.shutdown().await;
    }
}

#[tokio::test]
async fn manual_pause_http_resource_and_resume_rotate_credential_without_reusing_new_session_receipt()
 {
    let (host, old, h) = setup(true);
    let (status, _, _) = request(
        &host,
        Role::Operator,
        "GET",
        "/api/interpretation/state",
        &HeaderMap::new(),
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    let (status, _, body) = request(
        &host,
        Role::Operator,
        "POST",
        "/api/interpretation/pause",
        &h,
        json!({"generation":1}),
    )
    .await;
    assert_eq!(status, StatusCode::ACCEPTED);
    assert_eq!(body["schema_version"], "interpretation-lifecycle.v1");
    await_pause(&host, &old, &h).await;
    let (status, _, paused) = request(
        &host,
        Role::Operator,
        "GET",
        "/api/interpretation/state",
        &h,
        json!({}),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(paused["resume_allowed"], true);
    let command = json!({"previous_generation":1,"request_id":nonce('c')});
    let (status, response_headers, body) = request(
        &host,
        Role::Operator,
        "POST",
        "/api/interpretation/resume",
        &h,
        command.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["session"]["generation"], 2);
    assert!(!response_headers.contains_key("set-cookie"));
    let token = response_headers["x-pilot-session"].to_str().unwrap();
    assert_ne!(Some(token), auth::bearer(&h));
    assert!(
        host.pilot()
            .authorize(
                &headers(token, body["session"]["csrf"].as_str().unwrap()),
                Role::Operator,
                true
            )
            .is_ok()
    );
    assert_ne!(
        body["session"]["csrf"],
        old.session(&h, Role::Operator).unwrap()["csrf"]
    );
    let (status, _, replay) = request(
        &host,
        Role::Operator,
        "POST",
        "/api/interpretation/resume",
        &h,
        command.clone(),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(replay, body);
    let (status, _, _) = request(
        &host,
        Role::Operator,
        "POST",
        "/api/session/new",
        &h,
        command,
    )
    .await;
    assert_ne!(status, StatusCode::OK);
    host.shutdown().await;
}
