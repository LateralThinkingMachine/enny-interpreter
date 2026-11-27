use super::*;
use axum::{
    Json,
    body::Body,
    http::{HeaderMap, HeaderValue, Method, Request, header},
    response::{IntoResponse, Response},
};
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Control {
    generation: u64,
    request_id: String,
}
pub(crate) async fn dispatch(
    host: Arc<crate::session_host::SessionHost>,
    role: Role,
    request: Request<Body>,
) -> Response {
    let pilot = host.pilot();
    let origins = pilot.origins();
    let headers = request.headers().clone();
    let post = request.method() == Method::POST;
    let valid = role == Role::Operator
        && auth::single(&headers, "host") == Some(origins.host(Role::Operator))
        && request.uri().query().is_none()
        && (!post || auth::single(&headers, "content-type") == Some("application/json"))
        && (!post && !headers.contains_key("origin")
            || auth::single(&headers, "origin") == Some(origins.origin(Role::Operator)))
        && !headers.contains_key("forwarded")
        && !headers.contains_key("x-forwarded-host");
    let result = if !valid {
        Err(ApiError::Forbidden)
    } else {
        handle(host, pilot.clone(), headers, request).await
    };
    let mut response = match result {
        Ok(value) => Json(value).into_response(),
        Err(e) => (
            e.status(),
            Json(json!({"error":e.code(),"generation":pilot.generation()})),
        )
            .into_response(),
    };
    response
        .headers_mut()
        .insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    response.headers_mut().insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    response
        .headers_mut()
        .insert("referrer-policy", HeaderValue::from_static("no-referrer"));
    response
}
async fn handle(
    host: Arc<crate::session_host::SessionHost>,
    pilot: Arc<Pilot>,
    headers: HeaderMap,
    request: Request<Body>,
) -> Result<Value, ApiError> {
    let path = request.uri().path().to_owned();
    let method = request.method().clone();
    let limiter = if path == "/api/webrtc/stop" {
        &host.webrtc.control
    } else {
        &host.webrtc.ordinary
    };
    let _permit = limiter
        .clone()
        .try_acquire_owned()
        .map_err(|_| ApiError::Capacity)?;
    let authority = pilot.authorize(&headers, Role::Operator, method == Method::POST)?;
    let bytes = tokio::time::timeout(
        Duration::from_secs(2),
        axum::body::to_bytes(request.into_body(), 96 * 1024),
    )
    .await
    .map_err(|_| ApiError::Invalid)?
    .map_err(|_| ApiError::Invalid)?;
    match (method, path.as_str()) {
        (Method::GET, "/api/webrtc/status") if bytes.is_empty() => {
            Ok(host.webrtc.status(&pilot, &authority))
        }
        (Method::POST, "/api/webrtc/offer") => {
            let offer: Offer = serde_json::from_slice(&bytes).map_err(|_| ApiError::Invalid)?;
            let generation = offer.generation;
            let request_id = offer.request_id.clone();
            let answer = host.admit_webrtc(&pilot, authority.clone(), &headers, offer)?;
            let value = tokio::time::timeout(Duration::from_secs(19), answer)
                .await
                .map_err(|_| ApiError::Unavailable)?
                .map_err(|_| ApiError::Unavailable)??;
            pilot.authorize(&headers, Role::Operator, true)?;
            if !host
                .webrtc
                .answer_current(&pilot, &authority, generation, &request_id)
            {
                return Err(ApiError::Stopped);
            }
            Ok(value)
        }
        (Method::POST, "/api/webrtc/heartbeat" | "/api/webrtc/stop") => {
            let control: Control = serde_json::from_slice(&bytes).map_err(|_| ApiError::Invalid)?;
            if !valid_request_id(&control.request_id) {
                return Err(ApiError::Invalid);
            }
            host.webrtc.control(
                &pilot,
                &authority,
                control.generation,
                &control.request_id,
                path.ends_with("/stop"),
            )?;
            if path.ends_with("/stop")
                && let Some(receipt) = host.webrtc.cancellation_receipt(
                    &authority,
                    control.generation,
                    &control.request_id,
                )
            {
                return Ok(receipt);
            }
            Ok(host.webrtc.status(&pilot, &authority))
        }
        _ => Err(ApiError::Invalid),
    }
}
