use super::*;
use crate::{auth::LocalOrigins, config::Config};
use std::{fs, os::unix::fs::PermissionsExt};

fn configured(port: u16) -> (Arc<Pilot>, String) {
    Pilot::new_with_origins(
        engine::disabled(),
        Duration::from_secs(60),
        30,
        LocalOrigins::for_port(port).unwrap(),
    )
    .unwrap()
}

fn app(pilot: Arc<Pilot>, role: Role) -> axum::Router {
    let assets = PathBuf::from("/private/tmp/no-pilot-assets");
    match role {
        Role::Operator => crate::router(pilot, assets),
        Role::Public => crate::public_router(pilot, assets),
    }
}

async fn call(
    app: axum::Router,
    method: &str,
    path: &str,
    host: &str,
    origin: Option<&str>,
    identity: Option<&HeaderMap>,
    body: Value,
) -> (StatusCode, HeaderMap, Value) {
    let mut request = Request::builder()
        .method(method)
        .uri(path)
        .header("host", host)
        .header("content-type", "application/json");
    if let Some(origin) = origin {
        request = request.header("origin", origin);
    }
    if let Some(identity) = identity {
        for (name, value) in identity {
            request = request.header(name, value);
        }
    }
    let response = app
        .oneshot(request.body(Body::from(body.to_string())).unwrap())
        .await
        .unwrap();
    let status = response.status();
    let headers = response.headers().clone();
    let bytes = response.into_body().collect().await.unwrap().to_bytes();
    (status, headers, serde_json::from_slice(&bytes).unwrap())
}

#[test]
fn only_two_reviewed_origin_pairs_can_be_constructed() {
    assert_eq!(
        LocalOrigins::default(),
        LocalOrigins::for_port(5180).unwrap()
    );
    for port in [5080, 5180] {
        let origins = LocalOrigins::for_port(port).unwrap();
        for (role, expected) in [(Role::Operator, port), (Role::Public, port + 2)] {
            assert_eq!(origins.host(role), format!("127.0.0.1:{expected}"));
            assert_eq!(origins.origin(role), format!("http://127.0.0.1:{expected}"));
            assert!(
                origins
                    .content_security_policy(role)
                    .contains(&format!("connect-src 'self' ws://127.0.0.1:{expected};"))
            );
        }
    }
    for port in [0, 1, 80, 5079, 5081, 5082, 5179, 5181, 5182, 65535] {
        assert_eq!(LocalOrigins::for_port(port), Err("invalid_operator_port"));
    }
}

struct PrivateDirectory(PathBuf);
impl PrivateDirectory {
    fn new() -> Self {
        let path = PathBuf::from(format!(
            "/private/tmp/pilot-origins-test-{}",
            auth::secret().unwrap()
        ));
        fs::create_dir(&path).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o700)).unwrap();
        Self(path)
    }
}
impl Drop for PrivateDirectory {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.0).unwrap();
    }
}

#[test]
fn private_config_preserves_default_and_writes_selected_bootstrap_origin() {
    let directory = PrivateDirectory::new();
    let path = directory.0.join("config.json");
    let credentials = directory.0.join("credentials.json");
    let base = json!({"assets_dir":directory.0,"credentials_path":credentials,
        "duration_seconds":60,"max_input_audio_seconds":60});
    for port in [None, Some(5080), Some(5180)] {
        let mut value = base.clone();
        if let Some(port) = port {
            value["operator_port"] = json!(port);
        }
        fs::write(&path, value.to_string()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let config = Config::load(&path).unwrap();
        assert_eq!(config.operator_port, port.unwrap_or(5180));
        config.write_credentials(&"a".repeat(64)).unwrap();
        let output: Value = serde_json::from_slice(&fs::read(&credentials).unwrap()).unwrap();
        assert_eq!(
            output["origin"],
            config.origins().unwrap().origin(Role::Operator)
        );
        assert_eq!(output["expires_in_seconds"], 120);
        assert_eq!(
            fs::metadata(&credentials).unwrap().permissions().mode() & 0o777,
            0o600
        );
        fs::remove_file(&credentials).unwrap();
    }
    for invalid in [
        json!(0),
        json!(5082),
        json!(5182),
        json!(65535),
        json!(65536),
        json!(-1),
        json!(5080.0),
        json!(true),
        json!(null),
        json!("5080"),
    ] {
        let mut value = base.clone();
        value["operator_port"] = invalid;
        fs::write(&path, value.to_string()).unwrap();
        assert!(Config::load(&path).is_err());
        assert!(!credentials.exists());
    }
}

#[tokio::test]
async fn selected_pair_bootstrap_tickets_and_public_isolation_match() {
    for port in [5080, 5180] {
        let (pilot, ticket) = configured(port);
        let origins = pilot.origins();
        let operator_app = app(pilot.clone(), Role::Operator);
        let (status, response, view) = call(
            operator_app.clone(),
            "POST",
            "/api/bootstrap",
            origins.host(Role::Operator),
            Some(origins.origin(Role::Operator)),
            None,
            json!({"ticket":ticket}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["role"], "operator");
        assert!(!response.contains_key("set-cookie"));
        let token = response["x-pilot-session"].to_str().unwrap();
        let mut identity = HeaderMap::new();
        identity.insert(
            "authorization",
            HeaderValue::from_str(&format!("Bearer {token}")).unwrap(),
        );
        identity.insert(
            "x-pilot-csrf",
            HeaderValue::from_str(view["csrf"].as_str().unwrap()).unwrap(),
        );
        assert_eq!(
            response["content-security-policy"],
            origins.content_security_policy(Role::Operator)
        );
        let (status, _, ticket) = call(
            operator_app,
            "POST",
            "/api/public/tickets",
            origins.host(Role::Operator),
            Some(origins.origin(Role::Operator)),
            Some(&identity),
            json!({"generation":1}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(ticket["origin"], origins.origin(Role::Public));
        let public_app = app(pilot.clone(), Role::Public);
        let (status, response, view) = call(
            public_app.clone(),
            "POST",
            "/api/public/bootstrap",
            origins.host(Role::Public),
            Some(origins.origin(Role::Public)),
            None,
            json!({"ticket":ticket["ticket"]}),
        )
        .await;
        assert_eq!(status, StatusCode::OK);
        assert_eq!(view["role"], "public");
        assert_eq!(
            response["content-security-policy"],
            origins.content_security_policy(Role::Public)
        );
        assert!(!response.contains_key("set-cookie"));
        let public_token = response["x-pilot-session"].to_str().unwrap();
        assert_ne!(token, public_token);
        for path in [
            "/api/session",
            "/api/budget",
            "/api/diagnostics",
            "/api/operator/events",
        ] {
            assert_eq!(
                call(
                    public_app.clone(),
                    "GET",
                    path,
                    origins.host(Role::Public),
                    Some(origins.origin(Role::Public)),
                    Some(&identity),
                    json!({})
                )
                .await
                .0,
                StatusCode::FORBIDDEN
            );
        }
        assert!(!pilot.is_stopped());
    }
}

#[tokio::test]
async fn role_sessions_require_their_explicit_http_credential() {
    let (pilot, ticket) = configured(5180);
    let operator = operator(&pilot, &ticket);
    let public = viewer(&pilot);
    for (role, path, identity, other) in [
        (Role::Operator, "/api/session", &operator, &public),
        (Role::Public, "/api/public/session", &public, &operator),
    ] {
        let token = auth::bearer(identity).unwrap();
        let mut cookie_only = HeaderMap::new();
        let old_name = if role == Role::Operator {
            "pilot_operator"
        } else {
            "pilot_public"
        };
        cookie_only.insert("cookie", format!("{old_name}={token}").parse().unwrap());
        let mut ws_only = HeaderMap::new();
        ws_only.insert(
            "sec-websocket-protocol",
            format!("pilot.events.v1, pilot.auth.{token}")
                .parse()
                .unwrap(),
        );
        let mut duplicate = identity.clone();
        duplicate.append("authorization", identity["authorization"].clone());
        let app = app(pilot.clone(), role);
        for (proof, expected) in [
            (&HeaderMap::new(), StatusCode::UNAUTHORIZED),
            (&cookie_only, StatusCode::UNAUTHORIZED),
            (&ws_only, StatusCode::UNAUTHORIZED),
            (&duplicate, StatusCode::UNAUTHORIZED),
            (other, StatusCode::UNAUTHORIZED),
            (identity, StatusCode::OK),
        ] {
            let (status, response, value) = call(
                app.clone(),
                "GET",
                path,
                pilot.origins().host(role),
                None,
                Some(proof),
                json!({}),
            )
            .await;
            assert_eq!(status, expected);
            assert_eq!(value.get("csrf").is_some(), expected == StatusCode::OK);
            assert!(!response.contains_key("set-cookie"));
            assert!(!response.contains_key("x-pilot-session"));
        }
    }
}

#[test]
fn event_authority_requires_one_role_bound_protocol_offer() {
    let (pilot, ticket) = configured(5180);
    let operator = operator(&pilot, &ticket);
    let public = viewer(&pilot);
    for (role, identity, other) in [
        (Role::Operator, &operator, &public),
        (Role::Public, &public, &operator),
    ] {
        let token = auth::bearer(identity).unwrap();
        let other_token = auth::bearer(other).unwrap();
        assert!(pilot.authorize_events(identity, role).is_err());
        let mut proof = HeaderMap::new();
        for offer in [
            format!("pilot.auth.{token}"),
            format!("pilot.events.v1, pilot.auth.{other_token}"),
            format!("pilot.events.v1, pilot.auth.{token}, pilot.auth.{token}"),
            format!("unknown, pilot.auth.{token}"),
        ] {
            proof.insert("sec-websocket-protocol", offer.parse().unwrap());
            assert!(pilot.authorize_events(&proof, role).is_err());
        }
        let offer = format!("pilot.events.v1, pilot.auth.{token}")
            .parse::<HeaderValue>()
            .unwrap();
        proof.insert("sec-websocket-protocol", offer.clone());
        assert!(pilot.authorize_events(&proof, role).is_ok());
        proof.append("sec-websocket-protocol", offer.clone());
        assert!(pilot.authorize_events(&proof, role).is_err());
        proof.insert("sec-websocket-protocol", offer);
        proof.insert("authorization", identity["authorization"].clone());
        assert!(pilot.authorize_events(&proof, role).is_err());
    }
}

#[tokio::test]
async fn inactive_pair_host_and_origin_are_rejected_on_both_surfaces() {
    for port in [5080, 5180] {
        let (pilot, ticket) = configured(port);
        let identity = operator(&pilot, &ticket);
        let origins = pilot.origins();
        let inactive = LocalOrigins::for_port(if port == 5080 { 5180 } else { 5080 }).unwrap();
        for role in [Role::Operator, Role::Public] {
            let app = app(pilot.clone(), role);
            assert_eq!(
                call(
                    app.clone(),
                    "GET",
                    "/api/health",
                    origins.host(role),
                    None,
                    None,
                    json!({})
                )
                .await
                .0,
                StatusCode::OK
            );
            for (host, origin) in [
                (inactive.host(role), Some(origins.origin(role))),
                (origins.host(role), Some(inactive.origin(role))),
                (inactive.host(role), Some(inactive.origin(role))),
                (origins.host(role), Some("http://localhost:5080")),
                ("localhost:5080", Some(origins.origin(role))),
            ] {
                assert_eq!(
                    call(
                        app.clone(),
                        "GET",
                        "/api/health",
                        host,
                        origin,
                        Some(&identity),
                        json!({})
                    )
                    .await
                    .0,
                    StatusCode::FORBIDDEN
                );
            }
            let path = if role == Role::Operator {
                "/api/stop"
            } else {
                "/api/public/logout"
            };
            for origin in [None, Some(inactive.origin(role))] {
                assert_eq!(
                    call(
                        app.clone(),
                        "POST",
                        path,
                        origins.host(role),
                        origin,
                        Some(&identity),
                        json!({})
                    )
                    .await
                    .0,
                    StatusCode::FORBIDDEN
                );
            }
        }
        assert!(!pilot.is_stopped());
    }
}

#[tokio::test]
async fn actual_websocket_handshake_uses_only_selected_pair_on_both_surfaces() {
    use futures_util::StreamExt;
    use tokio_tungstenite::{
        client_async,
        tungstenite::{Error, client::IntoClientRequest},
    };
    for port in [5080, 5180] {
        for role in [Role::Public, Role::Operator] {
            let (pilot, ticket) = configured(port);
            let identity = operator(&pilot, &ticket);
            let identity = if role == Role::Public {
                viewer(&pilot)
            } else {
                identity
            };
            let origins = pilot.origins();
            let inactive = LocalOrigins::for_port(if port == 5080 { 5180 } else { 5080 }).unwrap();
            let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
            let address = listener.local_addr().unwrap();
            let app = app(pilot.clone(), role);
            let server = tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
            let path = if role == Role::Operator {
                "/api/operator/events"
            } else {
                "/api/public/events"
            };
            for (host, origin, allowed) in [
                (inactive.host(role), inactive.origin(role), false),
                (origins.host(role), inactive.origin(role), false),
                (inactive.host(role), origins.origin(role), false),
                (origins.host(role), origins.origin(role), true),
            ] {
                let mut request = format!("ws://{}{path}", origins.host(role))
                    .into_client_request()
                    .unwrap();
                request
                    .headers_mut()
                    .insert("host", HeaderValue::from_static(host));
                request
                    .headers_mut()
                    .insert("origin", HeaderValue::from_static(origin));
                let token = auth::bearer(&identity).unwrap();
                request.headers_mut().insert(
                    "sec-websocket-protocol",
                    format!("pilot.events.v1, pilot.auth.{token}")
                        .parse()
                        .unwrap(),
                );
                let connection = tokio::net::TcpStream::connect(address).await.unwrap();
                let result =
                    tokio::time::timeout(Duration::from_secs(2), client_async(request, connection))
                        .await
                        .unwrap();
                if allowed {
                    let (mut socket, response) = result.unwrap();
                    assert_eq!(response.status(), StatusCode::SWITCHING_PROTOCOLS);
                    assert_eq!(
                        response.headers()["sec-websocket-protocol"],
                        "pilot.events.v1"
                    );
                    assert!(!response.headers().contains_key("x-pilot-session"));
                    let hello = tokio::time::timeout(Duration::from_secs(2), socket.next())
                        .await
                        .unwrap()
                        .unwrap()
                        .unwrap();
                    let hello: Value = serde_json::from_str(hello.to_text().unwrap()).unwrap();
                    assert_eq!(hello["type"], "hello");
                    assert_eq!(
                        hello["role"],
                        if role == Role::Operator {
                            "operator"
                        } else {
                            "public"
                        }
                    );
                    socket.close(None).await.unwrap();
                } else {
                    assert!(
                        matches!(result, Err(Error::Http(response)) if response.status() == StatusCode::FORBIDDEN)
                    );
                    assert!(!pilot.is_stopped());
                }
            }
            pilot.stop("operator_stop");
            server.abort();
            assert!(server.await.unwrap_err().is_cancelled());
        }
    }
}
