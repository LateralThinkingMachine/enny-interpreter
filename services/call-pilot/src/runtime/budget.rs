//! Operator-only read model; quotations never grant admission.
use super::*;
use crate::budget::{BudgetSnapshot, BudgetState};

impl Pilot {
    pub fn budget(&self, headers: &HeaderMap) -> Result<BudgetSnapshot, ApiError> {
        let session_id = self.authorize(headers, Role::Operator, false)?;
        let memory = self.memory.lock().expect("pilot state");
        if memory
            .operator
            .as_ref()
            .is_none_or(|s| s.id != session_id || !s.matches(auth::bearer(headers)))
        {
            return Err(ApiError::Unauthorized);
        }
        let mut snapshot = self.backend.budget_snapshot();
        let now = Instant::now();
        if self.expired() {
            drop(memory);
            self.stop("deadline");
            return Err(ApiError::Unauthorized);
        }
        snapshot.generation = self.generation();
        if let Some(deadline) = self.deadline {
            let remaining = deadline.saturating_duration_since(now).as_secs();
            snapshot.remaining_seconds = Some(if snapshot.amounts.is_some() {
                snapshot
                    .remaining_seconds
                    .map_or(remaining, |provider| provider.min(remaining))
            } else {
                remaining
            });
        }
        if self.is_continuous() && snapshot.amounts.is_none() {
            snapshot.remaining_seconds = None;
        }
        // Preserve the provider's actual reservation quote even when the pilot
        // has a shorter deadline. An advisory read must never underquote it.
        for quote in &mut snapshot.routes {
            if quote.quote_seconds.is_some_and(|seconds| {
                snapshot
                    .remaining_seconds
                    .is_some_and(|remaining| seconds > remaining)
            }) {
                quote.can_reserve = false;
            }
        }
        if snapshot.state == BudgetState::Active
            && snapshot
                .remaining_seconds
                .is_some_and(|remaining| remaining <= 10)
        {
            snapshot.block(BudgetState::Expired);
        }
        if self.stopped.load(Ordering::Acquire) {
            snapshot.block(BudgetState::Stopped);
        } else if self.finishing.load(Ordering::Acquire) {
            snapshot.block(BudgetState::Finishing);
        }
        Ok(snapshot)
    }
}
