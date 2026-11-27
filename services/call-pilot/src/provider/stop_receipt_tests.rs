use super::*;
use crate::engine::{ProviderActivity, ProviderStopEvidence};

fn terminal(route: Route, closed_confirmed: bool) -> ProviderDiagnostic {
    ProviderDiagnostic {
        route,
        stage: DiagnosticStage::Closing,
        category: if closed_confirmed {
            DiagnosticCategory::Confirmed
        } else {
            DiagnosticCategory::CloseUnconfirmed
        },
        event_type: DiagnosticEvent::None,
        upstream_code: Default::default(),
        upstream_parameter: Default::default(),
        http_status: None,
        ready: true,
        closed_confirmed,
    }
}

#[test]
fn stop_evidence_requires_all_terminal_observations_and_separates_reservations() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let transcription = backend
        .shared
        .reserve(Some(Route::Transcription), 0)
        .unwrap();
    let live = backend.shared.reserve(Some(Route::Live), 0).unwrap();
    backend.revoke();
    let reserved = backend.budget_snapshot().amounts.unwrap().reserved_microusd;
    assert!(reserved > 0);
    assert_eq!(
        backend.stop_evidence(),
        ProviderStopEvidence {
            activity: ProviderActivity::Unconfirmed,
            reserved_microusd: Some(reserved)
        }
    );
    backend
        .shared
        .record_diagnostic(terminal(Route::Transcription, true));
    assert_eq!(
        backend.stop_evidence().activity,
        ProviderActivity::Unconfirmed
    );
    backend
        .shared
        .record_diagnostic(terminal(Route::Live, true));
    drop((transcription, live)); // incomplete spend/usage never erases a transport observation
    assert_eq!(
        backend.stop_evidence(),
        ProviderStopEvidence {
            activity: ProviderActivity::CloseObserved,
            reserved_microusd: Some(reserved)
        }
    );

    let uncertain = OpenAiBackend::new(config()).unwrap();
    let _stream = uncertain.shared.reserve(Some(Route::Live), 0).unwrap();
    uncertain
        .shared
        .record_diagnostic(terminal(Route::Live, false));
    uncertain.revoke();
    assert_eq!(
        uncertain.stop_evidence().activity,
        ProviderActivity::Unconfirmed
    );

    let speech = OpenAiBackend::new(config()).unwrap();
    let _tts = speech.shared.reserve(None, 6).unwrap();
    speech.revoke();
    assert_eq!(
        speech.stop_evidence().activity,
        ProviderActivity::Unconfirmed
    );
    assert!(speech.stop_evidence().reserved_microusd.unwrap() > 0);
}

#[test]
fn racing_finite_reservation_and_global_stop_cannot_report_empty_after_admission() {
    use std::sync::Barrier;
    for _ in 0..32 {
        let backend = Arc::new(OpenAiBackend::new(config()).unwrap());
        let (pilot, ticket) =
            crate::Pilot::new(backend.clone(), Duration::from_secs(60), 60).unwrap();
        let (_, token) = pilot.redeem(&ticket, crate::auth::Role::Operator).unwrap();
        let mut h = axum::http::HeaderMap::new();
        h.insert("authorization", format!("Bearer {token}").parse().unwrap());
        let barrier = Barrier::new(2);
        std::thread::scope(|scope| {
            let attempt = scope.spawn(|| {
                barrier.wait();
                backend.shared.reserve(Some(Route::Live), 0)
            });
            barrier.wait();
            pilot.stop("operator_stop");
            let receipt = pilot.stop_receipt(&h).unwrap();
            if attempt.join().unwrap().is_ok() {
                assert_eq!(receipt["provider_activity"], "unconfirmed");
                assert!(receipt["reserved_microusd"].as_u64().unwrap() > 0);
            } else {
                assert_eq!(receipt["provider_activity"], "none_admitted");
                assert_eq!(receipt["reserved_microusd"], 0);
            }
            assert!(backend.shared.reserve(Some(Route::Live), 0).is_err());
            assert!(backend.shared.reserve(None, 6).is_err());
        });
    }
}

#[tokio::test]
async fn asr_close_rejects_eof_without_peer_close_frame() {
    use futures_util::StreamExt;
    use tokio_tungstenite::tungstenite::Message;
    let (mut client, mut server) = pair(4096).await;
    let mut p = ready(Route::Transcription);
    let closing = transport::close(&mut client, &mut p, Instant::now() + Duration::from_secs(1));
    let peer = async move {
        assert!(matches!(server.next().await, Some(Ok(Message::Close(_)))));
        // Reading queues an automatic response. Drop without writing it.
        drop(server);
    };
    let (observed, ()) = tokio::join!(closing, peer);
    assert!(!observed);
}
