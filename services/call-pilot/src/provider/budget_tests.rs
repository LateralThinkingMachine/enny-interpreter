//! No transport/key lookup: direct local ledger invariants and quotations.
use super::*;
use crate::budget::{BudgetState, stream_cost};

#[test]
fn partial_minute_quotes_and_reservations_share_exact_integer_calculation() {
    for (route, expected) in [
        (Route::Transcription, 17284),
        (Route::Translation, 34567),
        (Route::Live, 50834),
    ] {
        let mut cfg = config();
        cfg.route_duration_seconds = 61;
        let backend = OpenAiBackend::new(cfg).unwrap();
        let before = backend.budget_snapshot();
        let quote = before.routes.iter().find(|q| q.route == route).unwrap();
        assert_eq!(quote.quote_seconds, Some(61));
        assert_eq!(quote.quote_microusd, Some(expected));
        let mut reservation = backend.shared.reserve(Some(route), 0).unwrap();
        let after = backend.budget_snapshot();
        assert_eq!(after.amounts.unwrap().reserved_microusd, expected);
        reservation.completed = true;
        drop(reservation);
        assert_eq!(
            backend.budget_snapshot().amounts.unwrap().reserved_microusd,
            expected,
            "Confirmed cleanup is never a refund"
        );
    }
    assert_eq!(stream_cost(Route::Live, 30), Some(25000));
    assert_eq!(stream_cost(Route::Live, 1), Some(834));
    assert_eq!(stream_cost(Route::Live, 0), Some(0));
    assert_eq!(stream_cost(Route::Live, u64::MAX), None);
}

#[test]
fn affordability_and_concurrent_admission_cannot_spend_one_quote_twice() {
    let mut cfg = config();
    cfg.approved_budget_microusd = 17000;
    cfg.allowed_routes = vec![Route::Transcription];
    cfg.allowed_live_modes = vec![];
    cfg.allow_tts = false;
    let backend = OpenAiBackend::new(cfg).unwrap();
    assert!(backend.supports(Route::Transcription));
    assert!(!backend.supports(Route::Translation));
    let barrier = Arc::new(std::sync::Barrier::new(8));
    let threads: Vec<_> = (0..8)
        .map(|_| {
            let shared = backend.shared.clone();
            let barrier = barrier.clone();
            std::thread::spawn(move || {
                barrier.wait();
                match shared.reserve(Some(Route::Transcription), 0) {
                    Ok(mut reservation) => {
                        reservation.completed = true;
                        true
                    }
                    Err(_) => false,
                }
            })
        })
        .collect();
    assert_eq!(
        threads
            .into_iter()
            .map(|t| usize::from(t.join().unwrap()))
            .sum::<usize>(),
        1
    );
    let state = backend.budget_snapshot();
    assert_eq!(state.stream_reservations.unwrap().used, 1);
    assert_eq!(state.amounts.unwrap().unallocated_microusd, 0);
    assert!(
        backend.supports(Route::Transcription),
        "Configuration remains separate from a spent reservation"
    );
    assert!(!backend.budget_snapshot().routes[0].can_reserve);
    let mut cfg = config();
    cfg.approved_budget_microusd = 1;
    let backend = OpenAiBackend::new(cfg).unwrap();
    assert!(
        backend.enabled(),
        "Configured availability alone is not affordability"
    );
    assert!(backend.supports(Route::Transcription));
    assert!(backend.supports_tts());
    assert!(
        backend
            .budget_snapshot()
            .routes
            .iter()
            .all(|q| !q.can_reserve)
    );
    assert!(!backend.budget_snapshot().tts.can_reserve_maximum);
    assert!(
        backend
            .shared
            .reserve(Some(Route::Transcription), 0)
            .is_err()
    );
    assert_eq!(
        backend.budget_snapshot().amounts.unwrap().reserved_microusd,
        0
    );
}

#[test]
fn configured_capability_is_stable_while_shorter_quotes_and_small_tts_fit() {
    let mut cfg = config();
    cfg.approved_budget_microusd = 300;
    let backend = OpenAiBackend::new(cfg).unwrap();
    assert!(backend.supports_tts());
    assert!(!backend.budget_snapshot().tts.can_reserve_maximum);
    let mut short = backend.shared.reserve(None, 10).unwrap();
    assert_eq!(
        backend.budget_snapshot().amounts.unwrap().reserved_microusd,
        300
    );
    short.completed = true;
    drop(short);
    assert!(backend.shared.reserve(None, 1).is_err());
    let mut cfg = config();
    cfg.approved_budget_microusd = 30000;
    let mut backend = OpenAiBackend::new(cfg).unwrap();
    assert!(backend.supports(Route::Live));
    assert!(!backend.budget_snapshot().routes[2].can_reserve);
    Arc::get_mut(&mut backend.shared).unwrap().deadline = Instant::now() + Duration::from_secs(30);
    assert!(backend.supports(Route::Live));
    let quote = backend.budget_snapshot();
    assert!(quote.routes[2].can_reserve);
    assert!(quote.routes[2].quote_microusd.unwrap() <= 25000);
    assert_eq!(quote.amounts.unwrap().reserved_microusd, 0);
}

#[test]
fn uncertainty_stops_quotes_without_inventing_refund_or_zero_balance() {
    let backend = OpenAiBackend::new(config()).unwrap();
    let reservation = backend.shared.reserve(Some(Route::Live), 0).unwrap();
    drop(reservation); // Missing terminal certainty poisons new work.
    let snapshot = backend.budget_snapshot();
    assert_eq!(snapshot.state, BudgetState::Uncertain);
    assert_eq!(snapshot.uncertain, Some(true));
    let amounts = snapshot.amounts.as_ref().unwrap();
    assert_eq!(amounts.reserved_microusd, 50000);
    assert_eq!(
        amounts.approved_microusd,
        amounts.reserved_microusd + amounts.unallocated_microusd
    );
    assert!(amounts.unallocated_microusd > 0);
    assert!(snapshot.routes.iter().all(|q| !q.can_reserve));
    assert!(!snapshot.tts.can_reserve_maximum);
    let encoded = serde_json::to_string(&snapshot).unwrap();
    assert!(!encoded.contains("synthetic-not-a-real-key"));
    assert!(!snapshot.invoice_verified && !snapshot.account_hard_cap_proven);
}

#[test]
fn expiry_stop_and_tts_count_have_explicit_read_only_states() {
    let mut backend = OpenAiBackend::new(config()).unwrap();
    let mut reservation = backend.shared.reserve(None, 3).unwrap();
    assert_eq!(
        backend.budget_snapshot().amounts.unwrap().reserved_microusd,
        90
    );
    assert!(!backend.budget_snapshot().tts.can_reserve_maximum);
    reservation.completed = true;
    drop(reservation);
    let tts = backend.budget_snapshot();
    assert!(tts.tts.can_reserve_maximum);
    assert_eq!(tts.tts_reservations.unwrap().used, 1);
    assert_eq!(
        tts.tts.max_input_utf8_bytes,
        protocol::MAX_TTS_TEXT_BYTES as u64
    );
    assert_eq!(
        tts.tts.per_input_utf8_byte_microusd,
        protocol::TTS_MICROUSD_PER_CHARACTER
    );
    Arc::get_mut(&mut backend.shared).unwrap().deadline = Instant::now() - Duration::from_secs(1);
    let expired = backend.budget_snapshot();
    assert_eq!(expired.state, BudgetState::Expired);
    assert_eq!(expired.routes[0].quote_seconds, Some(0));
    assert_eq!(expired.routes[0].quote_microusd, Some(0));
    assert!(expired.routes.iter().all(|q| !q.can_reserve));
    assert!(backend.shared.reserve(Some(Route::Live), 0).is_err());
    backend.shared.stop.send_replace(true);
    assert_eq!(backend.budget_snapshot().state, BudgetState::Stopped);
}

#[test]
fn desktop_streams_share_one_window_across_setup_staggered_start_and_expiry() {
    let config = ProviderConfig {
        total_duration_seconds: 3720,
        route_duration_seconds: 3600,
        allowed_routes: vec![Route::Transcription, Route::Live],
        max_sessions: 2,
        approved_budget_microusd: 4_020_000,
        ..config()
    };
    let backend = FiniteBackend::for_desktop_meeting(config).unwrap();
    let shared = &backend.shared;
    let startup = shared.deadline - Duration::from_secs(3720);
    let first = startup + Duration::from_secs(48);
    let end = first + Duration::from_secs(3600);
    // Use the same monotonic, lock-owned admission and budget paths as live
    // operation, advancing the clock without sleeping or opening any transport.
    let mut ledger = shared.state.lock().unwrap();
    assert!(
        shared
            .reserve_locked(&mut ledger, Some(Route::Translation), 0, startup)
            .is_err()
    );
    let before = shared.budget_locked(&ledger, first);
    assert_eq!(before.remaining_seconds, Some(3600));
    assert_eq!(before.amounts.unwrap().reserved_microusd, 0);
    assert_eq!(before.routes[2].quote_microusd, Some(3_000_000));
    assert!(matches!(ledger.window, StreamWindow::AwaitingFirstStream));
    let mut captions = shared
        .reserve_locked(&mut ledger, Some(Route::Transcription), 0, first)
        .unwrap();
    captions.completed = true;
    let later = first + Duration::from_millis(700_500);
    let during = shared.budget_locked(&ledger, later);
    assert_eq!(during.state, BudgetState::Active);
    assert_eq!(during.remaining_seconds, Some(2899));
    assert_eq!(during.routes[2].quote_seconds, Some(2899));
    assert!(during.routes[2].can_reserve);
    let mut live = shared
        .reserve_locked(&mut ledger, Some(Route::Live), 0, later)
        .unwrap();
    live.completed = true;
    assert_eq!(live.seconds, 2899);
    let delayed_poll = later + Duration::from_secs(5);
    assert_eq!(captions.stream_deadline(delayed_poll), end);
    assert_eq!(
        live.stream_deadline(delayed_poll),
        end - Duration::from_millis(500)
    );
    let spent = ledger.reserved;
    assert_eq!(spent, 1_020_000 + 2_415_834);
    let expired = shared.budget_locked(&ledger, end);
    assert_eq!(expired.state, BudgetState::Expired);
    assert_eq!(expired.remaining_seconds, Some(0));
    assert!(expired.routes.iter().all(|route| !route.can_reserve));
    assert!(matches!(
        shared.reserve_locked(&mut ledger, Some(Route::Live), 0, end),
        Err(ProviderError::Unavailable)
    ));
    assert_eq!(ledger.reserved, spent);
}

#[test]
fn desktop_insufficient_funds_never_starts_a_meeting() {
    let backend = FiniteBackend::for_desktop_meeting(ProviderConfig {
        approved_budget_microusd: 1,
        ..config()
    })
    .unwrap();
    assert!(matches!(
        backend.shared.reserve(Some(Route::Live), 0),
        Err(ProviderError::Budget)
    ));
    let state = backend.shared.state.lock().unwrap();
    assert!(matches!(state.window, StreamWindow::AwaitingFirstStream));
    assert_eq!((state.sessions, state.reserved), (0, 0));
    let late = backend.shared.deadline - Duration::from_secs(25);
    let preview = backend.shared.budget_locked(&state, late);
    assert_eq!(preview.remaining_seconds, Some(25));
    assert_eq!(preview.routes[2].quote_seconds, Some(25));
}

#[test]
fn operator_budget_offers_the_second_desktop_route_after_a_fractional_delay() {
    use crate::{Pilot, auth::Role};
    use axum::http::HeaderMap;

    let mut backend = FiniteBackend::for_desktop_meeting(ProviderConfig {
        total_duration_seconds: 3720,
        route_duration_seconds: 3600,
        approved_budget_microusd: 4_020_000,
        ..config()
    })
    .unwrap();
    let first = Instant::now() - Duration::from_millis(700_500);
    Arc::get_mut(&mut backend.shared).unwrap().deadline = first + Duration::from_secs(3720);
    {
        let mut ledger = backend.shared.state.lock().unwrap();
        let mut first_stream = backend
            .shared
            .reserve_locked(&mut ledger, Some(Route::Transcription), 0, first)
            .unwrap();
        first_stream.completed = true;
    }
    let (pilot, ticket) = Pilot::new(Arc::new(backend), Duration::from_secs(3720), 7200).unwrap();
    let (_, token) = pilot.redeem(&ticket, Role::Operator).unwrap();
    let mut headers = HeaderMap::new();
    headers.insert("authorization", format!("Bearer {token}").parse().unwrap());
    let snapshot = pilot.budget(&headers).unwrap();
    let live = snapshot
        .routes
        .iter()
        .find(|q| q.route == Route::Live)
        .unwrap();
    assert_eq!(live.quote_seconds, snapshot.remaining_seconds);
    assert!(live.can_reserve);
}
