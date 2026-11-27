//! Coherent local reservation snapshot; current commands still reserve afresh.
use super::*;
use crate::budget::{Amounts, BudgetSnapshot, BudgetState, Reservations};

impl Shared {
    pub(super) fn budget_snapshot(&self) -> BudgetSnapshot {
        let Ok(state) = self.state.lock() else {
            return BudgetSnapshot::inactive_for(self.vendor, BudgetState::Unavailable);
        };
        self.budget_locked(&state, Instant::now())
    }
    pub(super) fn budget_locked(&self, state: &Ledger, now: Instant) -> BudgetSnapshot {
        let end = self.window_end(state.window, now);
        let status = if state.uncertain {
            BudgetState::Uncertain
        } else if *self.stop.borrow() || self.revocation.revoked() || self.admission.revoked() {
            BudgetState::Stopped
        } else if now + Duration::from_secs(10) >= end {
            BudgetState::Expired
        } else {
            BudgetState::Active
        };
        let remaining = end.saturating_duration_since(now);
        let seconds = self.route_seconds(remaining);
        let mut result = BudgetSnapshot::inactive_for(self.vendor, status);
        result.uncertain = Some(state.uncertain);
        result.amounts = Some(Amounts {
            approved_microusd: self.config.approved_budget_microusd,
            reserved_microusd: state.reserved,
            unallocated_microusd: self
                .config
                .approved_budget_microusd
                .saturating_sub(state.reserved),
        });
        result.remaining_seconds = Some(remaining.as_secs());
        result.route_duration_seconds = Some(self.config.route_duration_seconds);
        result.stream_reservations = Some(Reservations {
            used: state.sessions,
            limit: self.config.max_sessions,
        });
        result.tts_reservations = Some(Reservations {
            used: state.tts,
            limit: self.config.max_tts_requests,
        });
        for quote in &mut result.routes {
            quote.configured = self.config.allowed_routes.contains(&quote.route);
            if quote.configured {
                quote.quote_seconds = Some(seconds);
                quote.quote_microusd = self.vendor.stream_cost(quote.route, seconds);
                quote.can_reserve = status == BudgetState::Active
                    && state.sessions < self.config.max_sessions
                    && quote.quote_microusd.is_some_and(|cost| {
                        state
                            .reserved
                            .checked_add(cost)
                            .is_some_and(|total| total <= self.config.approved_budget_microusd)
                    });
            }
        }
        result.tts.configured = self.config.allow_tts;
        result.tts.can_reserve_maximum = status == BudgetState::Active
            && self.config.allow_tts
            && !state.tts_active
            && state.tts < self.config.max_tts_requests
            && state
                .reserved
                .checked_add(result.tts.max_request_microusd)
                .is_some_and(|total| total <= self.config.approved_budget_microusd);
        result
    }
}
