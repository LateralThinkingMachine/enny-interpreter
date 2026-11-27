//! Valid unsolicited Live closure stays distinct from local Finish or Stop.
//! In-memory WebSockets and synthetic messages only; no provider or credentials.
use super::*;
use crate::engine::{ProviderEndReason, StreamInput};
use futures_util::{SinkExt, StreamExt};
use tokio_tungstenite::tungstenite::Message;

fn reasons() -> [(&'static str, ProviderEndReason, &'static str); 5] {
    [
        (
            "close_requested",
            ProviderEndReason::CloseRequested,
            "live_closed_requested",
        ),
        ("expired", ProviderEndReason::Expired, "live_expired"),
        ("content", ProviderEndReason::Content, "live_content_closed"),
        (
            "remote_hangup",
            ProviderEndReason::RemoteHangup,
            "live_remote_hangup",
        ),
        (
            "connection_lost",
            ProviderEndReason::ConnectionLost,
            "live_connection_lost",
        ),
    ]
}

fn closed(reason: &str) -> Value {
    json!({"type":"session.closed","session":{"id":"sess_synthetic","model":protocol::LIVE},
           "reason":reason,"usage":{"seconds":1}})
}

#[test]
fn close_requires_a_valid_live_start_and_matching_established_id() {
    let mut startup = resolved(Route::Live);
    startup["audio"]["format"]["rate"] = json!(8000);
    for invalid_start in [false, true] {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        if invalid_start {
            assert!(
                p.consume(json!({"type":"session.started","session":startup}))
                    .is_err()
            );
        }
        for id in [
            None,
            Some(Value::Null),
            Some(json!("")),
            Some(json!("sess_synthetic")),
        ] {
            let mut event = closed("close_requested");
            if let Some(id) = id {
                event["session"]["id"] = id;
            } else {
                event["session"].as_object_mut().unwrap().remove("id");
            }
            assert!(p.consume(event).is_err());
            assert!(!p.closed);
            assert_eq!(p.live_end_reason, None);
            assert_eq!(p.usage, 0.0);
        }
    }
    let mut p = ready(Route::Live);
    let mut wrong = closed("close_requested");
    wrong["session"]["id"] = json!("some_other_session");
    assert!(p.consume(wrong).is_err());
    assert!(!p.closed);
    assert!(p.consume(closed("close_requested")).is_ok() && p.closed);
}

#[test]
fn live_terminal_reason_requires_valid_identity_usage_and_exact_known_reason() {
    for (upstream, reason, code) in reasons() {
        let mut p = ready(Route::Live);
        assert!(p.consume(closed(upstream)).unwrap().is_empty());
        assert_eq!(p.live_end_reason, Some(reason));
        assert!(p.closed);
        assert_eq!(p.usage, 1.0);
        assert_eq!(ProviderError::SessionEnded(reason).code(), code);
        assert_eq!(
            serde_json::to_value(diagnostics::error_category(ProviderError::SessionEnded(
                reason
            )))
            .unwrap(),
            code
        );
    }
    let canary = "synthetic-private-terminal-detail";
    let mut cases = vec![];
    let mut wrong_id = closed("content");
    wrong_id["session"]["id"] = json!(canary);
    cases.push(wrong_id);
    let mut wrong_model = closed("content");
    wrong_model["session"]["model"] = json!(canary);
    cases.push(wrong_model);
    cases.push(closed(canary));
    let mut missing_usage = closed("content");
    missing_usage.as_object_mut().unwrap().remove("usage");
    cases.push(missing_usage);
    let mut excessive_usage = closed("content");
    excessive_usage["usage"]["seconds"] = json!(3601);
    cases.push(excessive_usage);
    let mut negative_usage = closed("content");
    negative_usage["usage"]["seconds"] = json!(-1);
    cases.push(negative_usage);
    for value in cases {
        let mut p = ready(Route::Live);
        assert!(p.consume(value).is_err());
        assert!(!p.closed);
        assert!(p.live_end_reason.is_none());
        assert!(!format!("{:?}", p.failure).contains(canary));
    }
    let mut p = ready(Route::Live);
    p.consume(json!({"type":"session.usage.updated","usage":{"seconds":2}}))
        .unwrap();
    assert_eq!(
        p.consume(closed("content")).err(),
        Some(ProviderError::UsageMismatch)
    );
    assert!(p.live_end_reason.is_none() && !p.closed);
}

#[tokio::test]
async fn live_unsolicited_close_after_output_preserves_each_specific_reason() {
    for (upstream, reason, _) in reasons() {
        let backend = OpenAiBackend::new(config()).unwrap();
        let shared = backend.shared.clone();
        let (mut client, mut server) = pair(65536).await;
        let (_input, input) = tokio::sync::mpsc::channel(8);
        let (events, mut output) = tokio::sync::mpsc::channel(32);
        let (_stop, mut stopped) = watch::channel(false);
        let mut global = shared.stop.subscribe();
        let task = tokio::spawn(async move {
            let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
            let result = transport::drive(
                &shared,
                &mut client,
                &mut p,
                input,
                &events,
                &mut stopped,
                &mut global,
                Instant::now() + Duration::from_secs(30),
            )
            .await;
            assert_eq!(result, Err((false, ProviderError::SessionEnded(reason))));
            assert_eq!(
                p.failure,
                Some((
                    DiagnosticEvent::SessionClosed,
                    diagnostics::error_category(ProviderError::SessionEnded(reason))
                ))
            );
            transport::publish_failure(&events, &result);
            assert!(
                transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1))
                    .await
            );
            assert!(
                !p.close_requested,
                "confirmed upstream close must not trigger another close command"
            );
        });
        let request = server.next().await.unwrap().unwrap().into_text().unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(&request).unwrap()["type"],
            "session.start"
        );
        server
            .send(Message::Text(
                json!({"type":"session.started","session":resolved(Route::Live)})
                    .to_string()
                    .into(),
            ))
            .await
            .unwrap();
        assert!(matches!(
            output.recv().await,
            Some(StreamEvent::Ready { .. })
        ));
        server.send(Message::Text(json!({"type":"session.output_transcript.delta","delta":"Synthetic interpreted phrase.","start_ms":0,"end_ms":100}).to_string().into())).await.unwrap();
        assert!(matches!(
            output.recv().await,
            Some(StreamEvent::LiveTranscript { .. })
        ));
        server
            .send(Message::Text(closed(upstream).to_string().into()))
            .await
            .unwrap();
        assert!(
            matches!(output.recv().await, Some(StreamEvent::Error(ProviderError::SessionEnded(found))) if found == reason)
        );
        task.await.unwrap();
        assert!(
            output.recv().await.is_none(),
            "unsolicited closure is never presented as Drained or renewed"
        );
        assert!(!matches!(server.next().await, Some(Ok(Message::Text(_)))));
    }
}

#[tokio::test]
async fn live_close_during_explicit_finish_is_not_an_unsolicited_failure() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let shared = backend.shared.clone();
    let (mut client, mut server) = pair(65536).await;
    let (input, input_rx) = tokio::sync::mpsc::channel(8);
    let (events, mut output) = tokio::sync::mpsc::channel(32);
    let (_stop, mut stopped) = watch::channel(false);
    let mut global = shared.stop.subscribe();
    let task = tokio::spawn(async move {
        let mut p = protocol::Protocol::new(Route::Live, Language::Ja);
        let result = transport::drive(
            &shared,
            &mut client,
            &mut p,
            input_rx,
            &events,
            &mut stopped,
            &mut global,
            Instant::now() + Duration::from_secs(30),
        )
        .await;
        assert!(result.is_ok());
        assert!(p.closed && p.failure.is_none());
        assert_eq!(p.live_end_reason, Some(ProviderEndReason::CloseRequested));
    });
    assert!(server.next().await.is_some());
    server
        .send(Message::Text(
            json!({"type":"session.started","session":resolved(Route::Live)})
                .to_string()
                .into(),
        ))
        .await
        .unwrap();
    assert!(matches!(
        output.recv().await,
        Some(StreamEvent::Ready { .. })
    ));
    input.send(StreamInput::FinishInput).await.unwrap();
    // A generated silence-clock append proves drive accepted the Finish barrier.
    let message = server.next().await.unwrap().unwrap().into_text().unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&message).unwrap()["type"],
        "session.input_audio.append"
    );
    server
        .send(Message::Text(closed("close_requested").to_string().into()))
        .await
        .unwrap();
    task.await.unwrap();
    assert!(!matches!(output.recv().await, Some(StreamEvent::Error(_))));
}
