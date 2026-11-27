//! Actual Axum resource/middleware, without binding a socket or opening providers.
use super::*;
use crate::{
    budget::BudgetState,
    provider::{OpenAiBackend, ProviderConfig},
};

fn backend() -> Arc<OpenAiBackend> {
    Arc::new(
        OpenAiBackend::new(ProviderConfig {
            api_key: "synthetic-secret-never-exposed-in-cost-api".into(),
            allowed_routes: vec![Route::Transcription, Route::Translation],
            allowed_live_modes: vec![],
            allow_tts: false,
            total_duration_seconds: 60,
            route_duration_seconds: 30,
            max_sessions: 4,
            max_tts_requests: 0,
            max_input_audio_seconds: 60,
            max_output_audio_seconds: 60,
            max_text_characters: 1000,
            approved_budget_microusd: 500000,
        })
        .unwrap(),
    )
}

async fn get(
    app: axum::Router,
    uri: &str,
    host: &str,
    authorization: Option<&HeaderValue>,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder().uri(uri).header("host", host);
    if let Some(authorization) = authorization {
        request = request.header("authorization", authorization);
    }
    let response = app
        .oneshot(request.body(Body::empty()).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.into_body().collect().await.unwrap().to_bytes();
    assert!(body.len() < 4096);
    (status, headers, serde_json::from_slice(&body).unwrap())
}

#[tokio::test]
async fn operator_cost_resource_is_closed_no_store_and_cannot_cross_public_surface() {
    let backend = backend();
    let (p, ticket) = pilot(backend.clone());
    let operator = operator(&p, &ticket);
    let public = viewer(&p);
    let app = crate::router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    let public_app = crate::public_router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    for (router, host, authorization, status) in [
        (app.clone(), auth::HOST, None, StatusCode::UNAUTHORIZED),
        (
            app.clone(),
            auth::HOST,
            Some(&public["authorization"]),
            StatusCode::UNAUTHORIZED,
        ),
        (
            public_app.clone(),
            auth::PUBLIC_HOST,
            Some(&public["authorization"]),
            StatusCode::FORBIDDEN,
        ),
        (
            public_app,
            auth::PUBLIC_HOST,
            Some(&operator["authorization"]),
            StatusCode::FORBIDDEN,
        ),
    ] {
        let (actual, headers, value) = get(router, "/api/budget", host, authorization).await;
        assert_eq!(actual, status);
        assert_eq!(headers["cache-control"], "no-store");
        assert!(value.get("amounts").is_none());
    }
    let before = backend.budget_snapshot();
    let (status, headers, value) = get(
        app.clone(),
        "/api/budget",
        auth::HOST,
        Some(&operator["authorization"]),
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(headers["cache-control"], "no-store");
    assert_eq!(value.as_object().unwrap().len(), 15);
    assert_eq!(value["version"], 1);
    assert_eq!(value["state"], "active");
    assert_eq!(value["routes"].as_array().unwrap().len(), 3);
    assert_eq!(value["routes"][0]["quote_microusd"], 8500);
    assert_eq!(value["routes"][2]["configured"], false);
    assert!(value["routes"][2]["quote_microusd"].is_null());
    assert!(!value.to_string().contains("synthetic-secret"));
    assert_eq!(value["amounts"]["reserved_microusd"], 0);
    assert_eq!(
        backend.budget_snapshot().stream_reservations.unwrap().used,
        before.stream_reservations.unwrap().used
    );
    assert_eq!(
        get(
            app.clone(),
            "/api/budget?route=live",
            auth::HOST,
            Some(&operator["authorization"])
        )
        .await
        .0,
        StatusCode::FORBIDDEN
    );
    p.stop("operator_stop");
    let (_, _, stopped) = get(
        app.clone(),
        "/api/budget",
        auth::HOST,
        Some(&operator["authorization"]),
    )
    .await;
    assert_eq!(stopped["state"], "stopped");
    assert_eq!(stopped["generation"], 2);
    assert_eq!(stopped["amounts"], value["amounts"]);
    assert!(
        stopped["routes"]
            .as_array()
            .unwrap()
            .iter()
            .all(|q| q["can_reserve"] == false)
    );
    p.logout(Role::Operator);
    assert_eq!(
        get(
            app,
            "/api/budget",
            auth::HOST,
            Some(&operator["authorization"])
        )
        .await
        .0,
        StatusCode::UNAUTHORIZED
    );
}

#[tokio::test]
async fn disabled_and_finishing_cost_reads_never_create_scope_or_routes() {
    let (p, ticket) = pilot(engine::disabled());
    let operator = operator(&p, &ticket);
    let snapshot = p.budget(&operator).unwrap();
    assert_eq!(snapshot.state, BudgetState::Disabled);
    assert!(snapshot.amounts.is_none() && snapshot.uncertain.is_none());
    assert_eq!(snapshot.routes[2].per_minute_microusd, 50000);
    assert!(
        snapshot
            .routes
            .iter()
            .all(|q| !q.configured && !q.can_reserve && q.quote_seconds.is_none())
    );
    let subscription = subscribe(&p, &operator, Role::Operator);
    p.finish_from_socket(1, &subscription.id, &subscription.session)
        .unwrap();
    let snapshot = p.budget(&operator).unwrap();
    assert_eq!(snapshot.state, BudgetState::Finishing);
    assert!(snapshot.amounts.is_none());
    assert!(snapshot.routes.iter().all(|q| !q.can_reserve));
    p.stop("operator_stop");
}

#[tokio::test]
async fn budget_resource_refuses_payload_methods_foreign_origin_and_expired_identity() {
    let (p, ticket) = pilot(backend());
    let operator = operator(&p, &ticket);
    let app = crate::router(p.clone(), PathBuf::from("/private/tmp/no-assets"));
    for (method, body, origin, expected) in [
        (
            "GET",
            "untrusted-budget=1000000",
            auth::ORIGIN,
            StatusCode::BAD_REQUEST,
        ),
        ("POST", "{}", auth::ORIGIN, StatusCode::METHOD_NOT_ALLOWED),
        ("PUT", "{}", auth::ORIGIN, StatusCode::METHOD_NOT_ALLOWED),
        (
            "GET",
            "",
            "https://untrusted.example",
            StatusCode::FORBIDDEN,
        ),
        ("GET", "", auth::PUBLIC_ORIGIN, StatusCode::FORBIDDEN),
    ] {
        let request = Request::builder()
            .method(method)
            .uri("/api/budget")
            .header("host", auth::HOST)
            .header("origin", origin)
            .header("content-type", "application/json")
            .header("authorization", &operator["authorization"])
            .body(Body::from(body))
            .unwrap();
        let response = app.clone().oneshot(request).await.unwrap();
        assert_eq!(response.status(), expected);
        assert_eq!(response.headers()["cache-control"], "no-store");
        let bytes = response.into_body().collect().await.unwrap().to_bytes();
        assert!(!String::from_utf8_lossy(&bytes).contains("1000000"));
    }
    assert_eq!(
        p.budget(&operator)
            .unwrap()
            .amounts
            .unwrap()
            .reserved_microusd,
        0
    );
    let (p, ticket) = Pilot::new(engine::disabled(), Duration::from_millis(10), 1).unwrap();
    let operator = super::operator(&p, &ticket);
    tokio::time::sleep(Duration::from_millis(15)).await;
    assert!(matches!(p.budget(&operator), Err(ApiError::Unauthorized)));
}

#[test]
fn shorter_pilot_deadline_blocks_admission_without_reducing_actual_provider_quote() {
    for (duration, expected_state) in [(20, BudgetState::Active), (5, BudgetState::Expired)] {
        let backend = backend();
        let (p, ticket) = Pilot::new(backend.clone(), Duration::from_secs(duration), 1).unwrap();
        let headers = operator(&p, &ticket);
        let snapshot = p.budget(&headers).unwrap();
        assert_eq!(snapshot.state, expected_state);
        assert!(snapshot.remaining_seconds.unwrap() <= duration);
        assert_eq!(snapshot.route_duration_seconds, Some(30));
        let transcription = &snapshot.routes[0];
        assert!(transcription.configured);
        assert_eq!(transcription.quote_seconds, Some(30));
        assert_eq!(transcription.quote_microusd, Some(8500));
        assert!(!transcription.can_reserve);
        assert!(snapshot.routes.iter().all(|quote| !quote.can_reserve));
        assert!(!snapshot.tts.can_reserve_maximum);
        assert_eq!(snapshot.amounts.unwrap().reserved_microusd, 0);
        // The provider itself still has this exact, longer quote. The runtime
        // must label it inadmissible rather than inventing a cheaper duration.
        let provider = backend.budget_snapshot();
        assert_eq!(provider.routes[0].quote_microusd, Some(8500));
        assert!(provider.routes[0].can_reserve);
        p.stop("operator_stop");
    }
}
